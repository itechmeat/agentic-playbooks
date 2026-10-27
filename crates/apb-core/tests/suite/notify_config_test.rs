//! "Notify me" settings: the machine section, the project section, a run's
//! request and how the three combine; the recipient check; the dashboard
//! link a notice carries; and a connector function's `people:` block.

use apb_core::config::{GlobalConfig, ServerConfig, project_notify};
use apb_core::connector::def::ConnectorDoc;
use apb_core::notify::{
    NotifyRequest, NotifySettings, NotifySource, NotifyTarget, ProjectNotify, Toggle, Transition,
    effective,
};

use crate::common::env_lock;

fn target(to: &str) -> NotifyTarget {
    NotifyTarget {
        connector: "telegram".into(),
        account: "me".into(),
        function: "send_message".into(),
        to: to.into(),
    }
}

fn machine(default: Toggle) -> NotifySettings {
    NotifySettings {
        default,
        target: Some(target("1")),
        on: Transition::ALL.to_vec(),
    }
}

#[test]
fn nothing_asked_follows_the_machine_default() {
    let none = ProjectNotify::default();
    assert_eq!(effective(&machine(Toggle::Off), &none, None), None);
    let on = effective(&machine(Toggle::On), &none, None).unwrap();
    assert_eq!(on.source, NotifySource::Machine);
    assert_eq!(on.on, Transition::ALL.to_vec());
    assert_eq!(on.target, Some(target("1")));
}

#[test]
fn a_project_turns_the_default_off_or_narrows_it_but_never_turns_it_on() {
    let off = ProjectNotify {
        enabled: Some(false),
        on: None,
    };
    assert_eq!(effective(&machine(Toggle::On), &off, None), None);
    let on_only = ProjectNotify {
        enabled: Some(true),
        on: None,
    };
    assert_eq!(effective(&machine(Toggle::Off), &on_only, None), None);
    let narrow = ProjectNotify {
        enabled: None,
        on: Some(vec![Transition::Finished, Transition::Review]),
    };
    let e = effective(&machine(Toggle::On), &narrow, None).unwrap();
    assert_eq!(e.on, vec![Transition::Review, Transition::Finished]);
    assert_eq!(e.source, NotifySource::Project);
}

#[test]
fn a_run_request_wins_over_the_project_and_the_machine() {
    let off = ProjectNotify {
        enabled: Some(false),
        on: None,
    };
    let req = NotifyRequest::On {
        target: Some(target("2")),
        on: Some(vec![Transition::Question]),
    };
    let e = effective(&machine(Toggle::Off), &off, Some(&req)).unwrap();
    assert_eq!(e.target, Some(target("2")));
    assert_eq!(e.on, vec![Transition::Question]);
    assert_eq!(e.source, NotifySource::Run);
    assert!(e.source.is_explicit());
    let none = ProjectNotify::default();
    assert_eq!(
        effective(&machine(Toggle::On), &none, Some(&NotifyRequest::Off)),
        None
    );
}

#[test]
fn a_request_without_a_target_borrows_the_machine_one_or_names_none() {
    let req = NotifyRequest::On {
        target: None,
        on: None,
    };
    let none = ProjectNotify::default();
    let e = effective(&machine(Toggle::Off), &none, Some(&req)).unwrap();
    assert_eq!(e.target, Some(target("1")));
    let bare = NotifySettings::default();
    assert_eq!(effective(&bare, &none, Some(&req)).unwrap().target, None);
}

#[test]
fn a_recipient_must_be_short_plain_and_never_credential_shaped() {
    assert!(target("123456789").validate().is_ok());
    assert!(target("ops@example.com").validate().is_ok());
    assert!(target("").validate().is_err());
    assert!(target(&"1".repeat(257)).validate().is_err());
    // A bot token pasted into the recipient by mistake.
    let err = target("ghp_0123456789abcdefABCDEF0123456789abcd")
        .validate()
        .unwrap_err();
    assert!(err.contains("credential"), "{err}");
    let mut bad = target("1");
    bad.connector = "Tele Gram".into();
    assert!(bad.validate().is_err());
}

#[test]
fn the_machine_section_loads_and_a_default_without_target_is_refused() {
    let _l = env_lock();
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", dir.path());
    }
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        "notify:\n  default: on\n  target: { connector: telegram, account: me, function: send_message, to: \"42\" }\n  on: [review, finished]\n",
    )
    .unwrap();
    let cfg = GlobalConfig::load().unwrap();
    assert_eq!(cfg.notify.default, Toggle::On);
    assert_eq!(cfg.notify.target, Some(target("42")));
    assert_eq!(
        cfg.notify.on,
        vec![Transition::Review, Transition::Finished]
    );

    std::fs::write(&path, "notify:\n  default: on\n").unwrap();
    let err = GlobalConfig::load().unwrap_err();
    assert!(err.contains("notify"), "{err}");

    std::fs::write(&path, "port: 7000\n").unwrap();
    let cfg = GlobalConfig::load().unwrap();
    assert_eq!(cfg.notify, NotifySettings::default());
    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[test]
fn the_project_section_turns_off_or_narrows_and_names_no_target() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        project_notify(root.path()).unwrap(),
        ProjectNotify::default()
    );
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let file = root.path().join(".apb/config.yaml");
    std::fs::write(&file, "notify:\n  enabled: false\n  on: [review]\n").unwrap();
    let p = project_notify(root.path()).unwrap();
    assert_eq!(p.enabled, Some(false));
    assert_eq!(p.on, Some(vec![Transition::Review]));
    std::fs::write(
        &file,
        "notify:\n  target: { connector: telegram, account: me, function: send_message, to: \"1\" }\n",
    )
    .unwrap();
    assert!(project_notify(root.path()).is_err());
    std::fs::write(&file, "notify:\n  on: []\n").unwrap();
    let err = project_notify(root.path()).unwrap_err();
    assert!(err.contains("names no transition"), "{err}");
}

#[test]
fn a_run_link_points_at_the_dashboard_run_page() {
    let local = ServerConfig::default();
    assert_eq!(
        local.run_url(7321, "ws1", "build-17"),
        "http://127.0.0.1:7321/#/run/ws1/build-17"
    );
    let public = ServerConfig {
        public_base_url: Some("https://apb.example.com/".into()),
        ..ServerConfig::default()
    };
    assert_eq!(
        public.run_url(7321, "w s", "r/1"),
        "https://apb.example.com/#/run/w%20s/r%2F1"
    );
}

fn doc(function: &str) -> Result<ConnectorDoc, String> {
    let yaml = format!("name: chat\nversion: 0.1.0\nfunctions:\n{function}");
    ConnectorDoc::from_yaml(&yaml, "chat").map_err(|e| e.to_string())
}

const SEND: &str = "  - name: send\n    description: d\n    method: POST\n    url: https://chat.example.com/send\n    body: \"{{args}}\"\n    args_schema: { type: object, properties: { to: { type: string }, text: { type: string }, n: { type: integer } } }\n";

#[test]
fn a_people_block_names_string_args_of_a_function_that_sends() {
    let ok = doc(&format!(
        "{SEND}    people: {{ to: to, text: text }}\n    idempotency: {{ mode: none }}\n"
    ))
    .unwrap();
    let f = ok.function("send").unwrap();
    assert!(f.carries_notices());
    assert_eq!(
        f.people.as_ref().unwrap().read_args().collect::<Vec<_>>(),
        ["text"]
    );

    let brief_only = doc(&format!("{SEND}    people: {{ text: text }}\n")).unwrap();
    assert!(!brief_only.function("send").unwrap().carries_notices());

    let not_string = doc(&format!("{SEND}    people: {{ text: n }}\n")).unwrap_err();
    assert!(not_string.contains("people.text"), "{not_string}");
    let missing = doc(&format!("{SEND}    people: {{ text: body }}\n")).unwrap_err();
    assert!(missing.contains("people.text"), "{missing}");
    let unknown = doc(&format!("{SEND}    people: {{ text: text, cc: to }}\n")).unwrap_err();
    assert!(unknown.contains("cc"), "{unknown}");
    let read = doc(&format!(
        "{}    people: {{ text: text }}\n",
        SEND.replace("method: POST", "read_only: true\n    method: POST")
    ))
    .unwrap_err();
    assert!(read.contains("sends nothing"), "{read}");
}

#[test]
fn one_arg_never_fills_two_people_roles() {
    let both = doc(&format!("{SEND}    people: {{ to: text, text: text }}\n")).unwrap_err();
    assert!(
        both.contains("both `people.text` and `people.to`"),
        "{both}"
    );
}

#[test]
fn a_function_that_requires_an_arg_a_notification_cannot_fill_carries_none() {
    let schema = "args_schema: { type: object, properties: { to: { type: string }, text: { type: string }, owner: { type: string } }, required: [to, text, owner] }";
    let send = SEND.replace(
        &SEND[SEND.find("args_schema").unwrap()..SEND.len() - 1],
        schema,
    );
    let f = doc(&format!(
        "{send}    people: {{ to: to, text: text }}\n    idempotency: {{ mode: none }}\n"
    ))
    .unwrap();
    assert!(!f.function("send").unwrap().carries_notices());
    let covered = SEND.replace(
        &SEND[SEND.find("args_schema").unwrap()..SEND.len() - 1],
        &schema.replace(", owner]", "]"),
    );
    let f = doc(&format!(
        "{covered}    people: {{ to: to, text: text }}\n    idempotency: {{ mode: none }}\n"
    ))
    .unwrap();
    assert!(f.function("send").unwrap().carries_notices());
}

#[test]
fn an_empty_transition_list_is_refused_where_it_is_written() {
    let mut empty = machine(Toggle::Off);
    empty.on.clear();
    assert!(
        empty
            .validate()
            .unwrap_err()
            .contains("names no transition")
    );
    let project = ProjectNotify {
        enabled: None,
        on: Some(vec![]),
    };
    assert!(project.validate().unwrap_err().contains("enabled: false"));
}

#[test]
fn a_request_narrowed_to_nothing_comes_back_for_the_caller_to_refuse() {
    let mut review_only = machine(Toggle::On);
    review_only.on = vec![Transition::Review];
    let finished_only = ProjectNotify {
        enabled: None,
        on: Some(vec![Transition::Finished]),
    };
    // A default narrowed to nothing is no subscription.
    assert_eq!(effective(&review_only, &finished_only, None), None);
    // A run that asked keeps its empty list.
    let asked = NotifyRequest::On {
        target: None,
        on: None,
    };
    let e = effective(&review_only, &finished_only, Some(&asked)).unwrap();
    assert!(e.on.is_empty());
    assert_eq!(e.source, NotifySource::Run);
}

#[test]
fn a_recipient_with_surrounding_whitespace_is_refused() {
    let err = target(" 123 ").validate().unwrap_err();
    assert!(err.contains("whitespace"), "{err}");
    assert!(target("-1001234567890").validate().is_ok());
}
