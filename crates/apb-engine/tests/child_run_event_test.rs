use apb_engine::event::{Event, EventPayload};

#[test]
fn child_run_started_roundtrips_and_defaults() {
    let e = Event {
        seq: 3,
        ts: 0,
        payload: EventPayload::ChildRunStarted {
            node_id: "c".into(),
            run_id: "child-1".into(),
        },
    };
    let line = serde_json::to_string(&e).unwrap();
    assert!(line.contains("\"type\":\"child_run_started\""));
    // Old logs with a bare variant still deserialize (all fields default).
    let bare: Event =
        serde_json::from_str("{\"seq\":0,\"ts\":0,\"type\":\"child_run_started\"}").unwrap();
    match bare.payload {
        EventPayload::ChildRunStarted { node_id, run_id } => {
            assert!(node_id.is_empty() && run_id.is_empty());
        }
        _ => panic!("wrong variant"),
    }
}
