use apb_engine::control::{Control, post_control, read_control_after};

#[test]
fn post_and_read_control_with_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let s0 = post_control(dir.path(), Control::Pause).unwrap();
    let s1 = post_control(
        dir.path(),
        Control::Retry {
            node: "impl".into(),
            prompt_override: Some("hint".into()),
        },
    )
    .unwrap();
    let s2 = post_control(
        dir.path(),
        Control::Abort {
            reason: "cancel".into(),
        },
    )
    .unwrap();
    assert_eq!((s0, s1, s2), (0, 1, 2));

    let all = read_control_after(dir.path(), None).unwrap();
    assert_eq!(all.len(), 3);
    assert!(matches!(all[0].cmd, Control::Pause));

    let tail = read_control_after(dir.path(), Some(0)).unwrap();
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].seq, 1);
    assert!(matches!(
        &tail[0].cmd,
        Control::Retry { node, prompt_override: Some(p) } if node == "impl" && p == "hint"
    ));

    // serialization: the cmd tag is in snake_case
    let raw = std::fs::read_to_string(dir.path().join("control.jsonl")).unwrap();
    assert!(!raw.contains("\"cmd\":\"continue_from\"")); // was not written
    assert!(raw.contains("\"cmd\":\"abort\""));
}

#[test]
fn read_missing_control_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_control_after(dir.path(), None).unwrap().is_empty());
}

// Wire compatibility of the targeted-interrupt change (spec 2026-08-05 section
// 1.6): `Control::Interrupt.node` is `#[serde(default)]`, so a control.jsonl line
// written by an older apb - reason only, no `node` key at all - still parses, and
// parses as the BROADCAST shape whose semantics that line was written under.
#[test]
fn a_legacy_interrupt_line_without_a_node_parses_as_broadcast() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("control.jsonl"),
        "{\"seq\":0,\"cmd\":\"interrupt\",\"reason\":\"attempt is wedged\"}\n",
    )
    .unwrap();

    let all = read_control_after(dir.path(), None).unwrap();
    assert_eq!(all.len(), 1, "the legacy line must parse, got: {all:?}");
    assert!(
        matches!(
            &all[0].cmd,
            Control::Interrupt { reason, node: None } if reason == "attempt is wedged"
        ),
        "a legacy interrupt must deserialize with no target, got: {:?}",
        all[0].cmd
    );
}

// The other direction of the same compatibility promise: a targeted interrupt
// serializes its node under the same flattened `cmd` envelope, so an older reader
// sees a line it can still parse (it ignores the unknown `node` key).
#[test]
fn a_targeted_interrupt_serializes_its_node() {
    let dir = tempfile::tempdir().unwrap();
    post_control(
        dir.path(),
        Control::Interrupt {
            reason: "branch is wedged".into(),
            node: Some("impl".into()),
        },
    )
    .unwrap();
    let raw = std::fs::read_to_string(dir.path().join("control.jsonl")).unwrap();
    assert!(raw.contains("\"cmd\":\"interrupt\""), "got {raw}");
    assert!(raw.contains("\"node\":\"impl\""), "got {raw}");
}

#[test]
fn progress_control_serializes_with_cmd_tag() {
    use apb_engine::control::Control;
    let c = Control::Progress {
        done: 3,
        total: 14,
        label: Some("chapter 3 of 14".into()),
    };
    let s = serde_json::to_string(&c).unwrap();
    assert!(s.contains("\"cmd\":\"progress\""), "got {s}");
    assert!(s.contains("\"done\":3"));
    assert!(s.contains("\"total\":14"));
}
