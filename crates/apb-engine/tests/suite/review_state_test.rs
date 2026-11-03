use apb_engine::event::{Event, EventPayload};
use apb_engine::state::RunState;

fn ev(seq: u64, p: EventPayload) -> Event {
    Event {
        seq,
        ts: 0,
        payload: p,
    }
}

#[test]
fn fold_records_review_decision() {
    let events = vec![
        ev(
            0,
            EventPayload::RunStarted {
                playbook: "apb".into(),
                version: "1.0.0".into(),
                definition: None,
            },
        ),
        ev(
            1,
            EventPayload::review_requested(
                "gate",
                vec!["approved".into(), "rejected".into()],
                String::new(),
            ),
        ),
        ev(
            2,
            EventPayload::review_decided("gate", "approved", "ok", None),
        ),
        ev(
            3,
            EventPayload::node_finished("gate", "succeeded", 1, "approved"),
        ),
    ];
    let state = RunState::fold(&events);
    let review = state.reviews.get("gate").expect("review decision recorded");
    assert_eq!(review.decision, "approved");
    assert_eq!(review.note, "ok");
}
