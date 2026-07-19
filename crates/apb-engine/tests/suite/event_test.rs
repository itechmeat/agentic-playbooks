use apb_engine::event::{EventLog, EventPayload, read_all};

#[test]
fn appends_and_reads_events_with_increasing_seq() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::create(dir.path()).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "ping".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(EventPayload::NodeFinished {
        node: "start".into(),
        status: "succeeded".into(),
        attempt: 1,
        output: String::new(),
    })
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
            ..
        }
    ));
}
