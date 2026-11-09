use apb_engine::event::{EventLog, EventPayload, read_all};

#[test]
fn appends_and_reads_events_with_increasing_seq() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::create(dir.path()).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "ping".into(),
        version: "1.0.0".into(),
        definition: None,
        external_data: false,
    })
    .unwrap();
    log.append(EventPayload::node_finished(
        "start",
        "succeeded",
        1,
        String::new(),
    ))
    .unwrap();

    let events = read_all(dir.path()).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 0);
    assert_eq!(events[1].seq, 1);
    assert!(matches!(events[0].payload, EventPayload::RunStarted { .. }));
    // serialization tags the type in snake_case
    let json = serde_json::to_value(&events[1]).unwrap();
    assert_eq!(json["type"], "node_finished");
    assert_eq!(json["node"], "start");
}

#[test]
fn open_continues_seq_for_resume() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::create(dir.path()).unwrap();
        log.append(EventPayload::RunStarted {
            playbook: "w".into(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        })
        .unwrap();
    }
    let mut log = EventLog::open(dir.path()).unwrap();
    let ev = log
        .append(EventPayload::RunFinished {
            outcome: "succeeded".into(),
        })
        .unwrap();
    assert_eq!(ev.seq, 1);
    assert_eq!(read_all(dir.path()).unwrap().len(), 2);
}

#[test]
fn connector_call_smtp_fields_round_trip() {
    let p = EventPayload::ConnectorCall {
        item: None,
        response_complete: None,
        node_id: "n".into(),
        connector: "smtp".into(),
        function: "send_email".into(),
        account: "acct1".into(),
        url: "smtp://smtp.example.com:587".into(),
        outcome: "ok".into(),
        http_status: None,
        duration_ms: 12,
        smtp_subject: Some("Hi".into()),
        smtp_recipients: Some(2),
        attempt: Some(2),
        attempt_seq: Some(17),
        intent: Some("si-abc".into()),
        origin: None,
    };
    let s = serde_json::to_string(&p).unwrap();
    assert!(!s.contains("password"));
    let back: EventPayload = serde_json::from_str(&s).unwrap();
    assert_eq!(format!("{back:?}"), format!("{p:?}"));
}

#[test]
fn old_connector_call_without_smtp_fields_still_parses() {
    // A log line written before this slice: the new fields default to None.
    let json = r#"{"type":"connector_call","node_id":"n","connector":"c","function":"f","account":"a","url":"","outcome":"ok","http_status":null,"duration_ms":1}"#;
    let p: EventPayload = serde_json::from_str(json).unwrap();
    assert!(matches!(
        p,
        EventPayload::ConnectorCall {
            smtp_subject: None,
            smtp_recipients: None,
            attempt: None,
            attempt_seq: None,
            intent: None,
            origin: None,
            ..
        }
    ));
}

#[test]
fn join_input_revived_has_its_own_tag_and_defaults_every_field() {
    let p = EventPayload::JoinInputRevived {
        node: "j".into(),
        sources: vec!["c2".into()],
        from_node: "c".into(),
    };
    let s = serde_json::to_string(&p).unwrap();
    assert!(s.contains(r#""type":"join_input_revived""#), "{s}");
    assert!(!p.is_checkpoint());
    let bare: EventPayload = serde_json::from_str(r#"{"type":"join_input_revived"}"#).unwrap();
    assert!(matches!(
        bare,
        EventPayload::JoinInputRevived { node, sources, from_node }
            if node.is_empty() && sources.is_empty() && from_node.is_empty()
    ));
}

#[test]
fn nodes_revived_has_its_own_tag_and_defaults_every_field() {
    let p = EventPayload::NodesRevived {
        from_node: "c".into(),
        nodes: vec!["c".into(), "j".into()],
        superseded: Vec::new(),
    };
    let s = serde_json::to_string(&p).unwrap();
    assert!(s.contains(r#""type":"nodes_revived""#), "{s}");
    assert!(!p.is_checkpoint());
    let back: EventPayload = serde_json::from_str(&s).unwrap();
    assert_eq!(format!("{back:?}"), format!("{p:?}"));
    let bare: EventPayload = serde_json::from_str(r#"{"type":"nodes_revived"}"#).unwrap();
    assert!(matches!(
        bare,
        EventPayload::NodesRevived { from_node, nodes, superseded } if from_node.is_empty() && nodes.is_empty() && superseded.is_empty()
    ));
}

/// The result stamp on both settlement events and the attempt heads on
/// `artifacts_committed`: an older line without them reads as absent and
/// writes back byte for byte (the journal is compared line by line), a
/// present value round-trips, and a shape a newer apb wrote reads as
/// absent instead of failing the journal.
#[test]
fn result_stamps_and_attempt_heads_are_additive_and_lenient() {
    use apb_engine::event::Event;
    let decode = |line: &str| serde_json::from_str::<Event>(line).unwrap();
    let old = [
        r#"{"seq":1,"ts":5,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","artifacts":[]}"#,
        r#"{"seq":2,"ts":5,"type":"list_item_settled","node":"l","item":"a","state":"passed","round":0}"#,
        r#"{"seq":3,"ts":5,"type":"artifacts_committed","node":"w","before":"a","after":"b","commits":[]}"#,
    ];
    for line in old {
        assert_eq!(serde_json::to_string(&decode(line)).unwrap(), line);
    }
    let stamp =
        r#""result":{"snapshot_id":"ab","entries":2,"bytes":40,"omitted":1,"incomplete":true}"#;
    let new = [
        format!(
            r#"{{"seq":1,"ts":5,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","artifacts":[],{stamp}}}"#
        ),
        format!(
            r#"{{"seq":2,"ts":5,"type":"list_item_settled","node":"l","item":"a","state":"passed","round":0,{stamp}}}"#
        ),
        r#"{"seq":3,"ts":5,"type":"artifacts_committed","node":"w","before":"a","after":"b","commits":[],"attempt":9,"attempts":[{"attempt":4,"head":"c"},{"attempt":9,"head":"b"}]}"#.to_string(),
    ];
    for line in &new {
        assert_eq!(&serde_json::to_string(&decode(line)).unwrap(), line);
    }
    match decode(&new[0]).payload {
        EventPayload::NodeFinished {
            result: Some(r), ..
        } => {
            assert_eq!((r.snapshot_id.as_str(), r.entries, r.bytes), ("ab", 2, 40));
            assert!(r.incomplete && r.omitted == 1);
        }
        other => panic!("{other:?}"),
    }
    match decode(&new[2]).payload {
        EventPayload::ArtifactsCommitted {
            attempt, attempts, ..
        } => {
            assert_eq!(attempt, Some(9));
            assert_eq!(
                attempts.iter().map(|a| a.attempt).collect::<Vec<_>>(),
                [4, 9]
            );
        }
        other => panic!("{other:?}"),
    }
    let newer = [
        r#"{"seq":1,"ts":5,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","result":{"snapshot":["ab"]}}"#,
        r#"{"seq":2,"ts":5,"type":"list_item_settled","node":"l","item":"a","state":"passed","result":"ab"}"#,
        r#"{"seq":3,"ts":5,"type":"artifacts_committed","node":"w","attempts":{"4":"c"}}"#,
    ];
    for line in newer {
        match decode(line).payload {
            EventPayload::NodeFinished { result, .. }
            | EventPayload::ListItemSettled { result, .. } => assert_eq!(result, None, "{line}"),
            EventPayload::ArtifactsCommitted { attempts, .. } => assert!(attempts.is_empty()),
            other => panic!("{other:?}"),
        }
    }
}
