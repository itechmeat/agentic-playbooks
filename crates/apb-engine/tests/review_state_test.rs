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
            },
        ),
        ev(
            1,
            EventPayload::ReviewRequested {
                node: "gate".into(),
                options: vec!["approved".into(), "rejected".into()],
            },
        ),
        ev(
            2,
            EventPayload::ReviewDecided {
                node: "gate".into(),
                decision: "approved".into(),
                note: "ok".into(),
            },
        ),
        ev(
            3,
            EventPayload::NodeFinished {
                node: "gate".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: "approved".into(),
            },
        ),
    ];
    let state = RunState::fold(&events);
    let review = state.reviews.get("gate").expect("review decision recorded");
    assert_eq!(review.decision, "approved");
    assert_eq!(review.note, "ok");
}
