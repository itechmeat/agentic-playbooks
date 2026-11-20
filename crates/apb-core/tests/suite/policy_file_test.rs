//! `policy.yaml`: the strict reader of both files, the filters a default
//! notification subscription takes from them, how they combine with a
//! run's own request, the journal names a filtered `finished` carries, and
//! the bounded reader every settings file goes through.

use apb_core::notify::{
    Coverage, FinishOutcome, NotifyRequest, NotifySettings, NotifySource, NotifyTarget,
    ProjectNotify, Settings, Toggle, Transition, effective,
};
use apb_core::policy_file::{
    PlaybookKey, Policies, PolicyFile, PolicySource, doctor_line, parse, parse_as, project_path,
};

use crate::common::config_sandbox;

fn machine_on() -> NotifySettings {
    NotifySettings {
        default: Toggle::On,
        target: Some(NotifyTarget {
            connector: "chat".into(),
            account: "me".into(),
            function: "send".into(),
            to: "1".into(),
        }),
        on: Transition::DEFAULT.to_vec(),
    }
}

fn policies(machine: &str, project: &str) -> Policies {
    Policies {
        machine: parse(machine).unwrap(),
        project: parse(project).unwrap(),
    }
}

const DEPLOY: PlaybookKey<'static> = PlaybookKey {
    id: "deploy",
    global: false,
};
const TRIAGE_GLOBAL: PlaybookKey<'static> = PlaybookKey {
    id: "triage",
    global: true,
};

fn eff(
    policies: &Policies,
    playbook: &PlaybookKey,
    request: Option<&NotifyRequest>,
) -> Option<apb_core::notify::Effective> {
    effective(
        Settings {
            machine: &machine_on(),
            project: &ProjectNotify::default(),
            policies,
        },
        Some(playbook),
        request,
    )
}

#[test]
fn a_valid_file_reads_its_notify_filters() {
    let file =
        parse("notify:\n  finished: [failed, aborted]\n  playbooks: [deploy, global:triage]\n")
            .unwrap();
    assert_eq!(
        file.notify.finished,
        Some(vec![FinishOutcome::Failed, FinishOutcome::Aborted])
    );
    assert!(file.notify.covers(&DEPLOY));
    assert!(file.notify.covers(&TRIAGE_GLOBAL));
    assert!(!file.notify.covers(&PlaybookKey {
        id: "triage",
        global: false
    }));
    // Empty and absent read as nothing set.
    assert_eq!(parse("").unwrap(), PolicyFile::default());
    assert_eq!(parse("# nothing\n").unwrap(), PolicyFile::default());
    assert_eq!(parse("notify: {}\n").unwrap(), PolicyFile::default());
}

#[test]
fn an_unknown_section_key_or_value_is_an_error_that_names_it() {
    let err = parse("schedules:\n  nightly: on\n").unwrap_err();
    assert!(err.contains("schedules"), "{err}");
    let err = parse("notify:\n  outcome: [failed]\n").unwrap_err();
    assert!(err.contains("outcome"), "{err}");
    let err = parse("notify:\n  finished: [crashed]\n").unwrap_err();
    assert!(err.contains("crashed"), "{err}");
    let err = parse("notify:\n  playbooks: [\"../x\"]\n").unwrap_err();
    assert!(err.contains("../x"), "{err}");
    let err = parse("notify:\n  finished: []\n").unwrap_err();
    assert!(err.contains("no outcome"), "{err}");
    let err = parse("billing:\n  metered: allow\n").unwrap_err();
    assert!(err.contains("metered"), "{err}");
    let err = parse("billing:\n  billed: sometimes\n").unwrap_err();
    assert!(err.contains("sometimes"), "{err}");
    let ok = parse(
        "billing:\n  billed: deny\n  unknown_basis: allow\n  budget_counts_subscription: false\n",
    )
    .unwrap();
    assert_eq!(
        ok.billing.billed,
        Some(apb_core::launch::LaunchPolicy::Deny)
    );
}

#[test]
fn a_damaged_file_does_not_read_and_names_its_file() {
    let _sandbox = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    assert_eq!(Policies::load(root.path()).unwrap(), Policies::default());

    std::fs::write(project_path(root.path()), "notify: [unclosed\n").unwrap();
    let err = Policies::load(root.path()).unwrap_err();
    assert_eq!(err.source, PolicySource::Project);
    assert!(err.message.contains("policy.yaml"), "{err}");
    let (ok, line) = doctor_line(root.path());
    assert!(!ok);
    assert!(
        line.contains("no default notification subscription"),
        "{line}"
    );

    std::fs::write(project_path(root.path()), "- a list\n").unwrap();
    assert!(Policies::load(root.path()).is_err());

    std::fs::write(project_path(root.path()), "notify:\n  finished: [failed]\n").unwrap();
    let machine = apb_core::policy_file::machine_path().unwrap();
    std::fs::write(&machine, "notify:\n  playbooks: [deploy]\n").unwrap();
    let loaded = Policies::load(root.path()).unwrap();
    assert_eq!(loaded.finished(), vec![FinishOutcome::Failed]);
    let (ok, line) = doctor_line(root.path());
    assert!(ok, "{line}");
    assert!(line.contains("notify.playbooks [deploy]"), "{line}");
    std::fs::write(&machine, "notify:\n  playbook: [deploy]\n").unwrap();
    let err = Policies::load(root.path()).unwrap_err();
    assert_eq!(err.source, PolicySource::Machine);
    assert!(err.message.contains("playbook"), "{err}");
}

#[test]
fn a_default_subscription_follows_both_playbook_filters() {
    let p = policies("notify:\n  playbooks: [deploy, global:triage]\n", "");
    assert!(eff(&p, &DEPLOY, None).is_some());
    assert!(eff(&p, &TRIAGE_GLOBAL, None).is_some());
    let other = PlaybookKey {
        id: "other",
        global: false,
    };
    assert_eq!(eff(&p, &other, None), None);
    // The project narrows: only what both pass.
    let p = policies(
        "notify:\n  playbooks: [deploy, global:triage]\n",
        "notify:\n  playbooks: [global:triage]\n",
    );
    assert_eq!(eff(&p, &DEPLOY, None), None);
    assert_eq!(
        eff(&p, &TRIAGE_GLOBAL, None).unwrap().source,
        NotifySource::Machine
    );
}

#[test]
fn an_explicit_request_is_not_filtered_by_playbook_and_wins_for_what_it_names() {
    let p = policies("notify:\n  finished: [failed]\n  playbooks: [deploy]\n", "");
    let other = PlaybookKey {
        id: "other",
        global: false,
    };
    let asked = eff(&p, &other, Some(&NotifyRequest::plain_on())).unwrap();
    assert_eq!(asked.source, NotifySource::Run);
    // Not named: the settings' filter.
    assert_eq!(asked.finished, vec![FinishOutcome::Failed]);
    let named = NotifyRequest::On {
        target: None,
        on: None,
        finished: Some(vec![FinishOutcome::Succeeded, FinishOutcome::Failed]),
    };
    let asked = eff(&p, &other, Some(&named)).unwrap();
    assert_eq!(
        asked.finished,
        vec![FinishOutcome::Succeeded, FinishOutcome::Failed]
    );
}

#[test]
fn the_project_narrows_the_machine_outcomes_and_none_left_drops_finished() {
    let p = policies(
        "notify:\n  finished: [failed, aborted]\n",
        "notify:\n  finished: [aborted, succeeded]\n",
    );
    let e = eff(&p, &DEPLOY, None).unwrap();
    assert_eq!(e.finished, vec![FinishOutcome::Aborted]);
    assert!(e.on.contains(&Transition::Finished));
    let p = policies(
        "notify:\n  finished: [failed]\n",
        "notify:\n  finished: [succeeded]\n",
    );
    let e = eff(&p, &DEPLOY, None).unwrap();
    assert!(e.finished.is_empty());
    assert!(!e.on.contains(&Transition::Finished), "{e:?}");
}

#[test]
fn journal_names_carry_finished_plainly_or_per_outcome_and_read_back() {
    let all = Coverage::new(
        &[Transition::Review, Transition::Finished],
        &FinishOutcome::ALL,
    );
    assert_eq!(all.journal_names(), ["review", "finished"]);
    let filtered = Coverage::new(
        &[Transition::Finished, Transition::HostTask],
        &[FinishOutcome::Failed],
    );
    assert_eq!(filtered.journal_names(), ["finished:failed", "host_task"]);
    assert_eq!(
        Coverage::from_journal_names(&filtered.journal_names()),
        filtered
    );
    assert_eq!(Coverage::from_journal_names(&all.journal_names()), all);
    // A name this apb does not know is ignored.
    let read = Coverage::from_journal_names(&["finished:someday", "review", "later"]);
    assert_eq!(read.on, vec![Transition::Review]);
}

#[test]
fn files_that_each_read_but_leave_no_outcome_together_are_refused() {
    let _sandbox = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let machine = apb_core::policy_file::machine_path().unwrap();
    std::fs::write(&machine, "notify:\n  finished: [failed]\n").unwrap();
    std::fs::write(
        project_path(root.path()),
        "notify:\n  finished: [succeeded]\n",
    )
    .unwrap();
    let err = Policies::load(root.path()).unwrap_err();
    assert_eq!(err.source, PolicySource::Project);
    assert!(err.message.contains("no outcome"), "{err}");
    let (ok, line) = doctor_line(root.path());
    assert!(!ok, "{line}");
}

#[test]
fn a_settings_file_must_be_a_small_regular_file() {
    let _sandbox = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let policy = project_path(root.path());
    let config = root.path().join(".apb/config.yaml");
    // Too large.
    let big = format!("# {}\n", "x".repeat(2 << 20));
    for path in [&policy, &config] {
        std::fs::write(path, &big).unwrap();
    }
    let err = Policies::load(root.path()).unwrap_err();
    assert!(err.message.contains("larger than"), "{err}");
    let err = apb_core::config::project_notify(root.path()).unwrap_err();
    assert!(err.contains("larger than"), "{err}");
    // A directory in the file's place is refused, not read as absent.
    for path in [&policy, &config] {
        std::fs::remove_file(path).unwrap();
        std::fs::create_dir(path).unwrap();
    }
    let err = Policies::load(root.path()).unwrap_err();
    assert!(err.message.contains("not a regular file"), "{err}");
    assert!(apb_core::config::project_notify(root.path()).is_err());
    // No `.apb` directory at all (a file in its place) sets nothing.
    let bare = tempfile::tempdir().unwrap();
    std::fs::write(bare.path().join(".apb"), "").unwrap();
    assert_eq!(Policies::load(bare.path()).unwrap(), Policies::default());
    assert!(apb_core::config::project_notify(bare.path()).is_ok());
    for path in [&policy, &config] {
        std::fs::remove_dir(path).unwrap();
        std::fs::write(path, "").unwrap();
    }
    // A link to a device never blocks a read: it is refused.
    #[cfg(unix)]
    for path in [&policy, &config] {
        std::fs::remove_file(path).unwrap();
        std::os::unix::fs::symlink("/dev/zero", path).unwrap();
    }
    #[cfg(unix)]
    {
        let err = Policies::load(root.path()).unwrap_err();
        assert!(err.message.contains("not a regular file"), "{err}");
        let err = apb_core::config::project_notify(root.path()).unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");
    }
}

#[test]
fn the_unattended_section_reads() {
    let file = parse("unattended:\n  after_attended_successes: 3\n").unwrap();
    assert_eq!(file.unattended.after_attended_successes, Some(3));
    assert!(!file.unattended.waive_probation);
    // A value outside 1 to 100 is refused.
    let err = parse("unattended:\n  after_attended_successes: 0\n").unwrap_err();
    assert!(
        err.contains("unattended.after_attended_successes is 0; use 1 to 100"),
        "{err}"
    );
    let err = parse("unattended:\n  after_attended_successes: 101\n").unwrap_err();
    assert!(err.contains("use 1 to 100"), "{err}");
    // An unknown key is refused (the strict reader).
    let err = parse("unattended:\n  waive: true\n").unwrap_err();
    assert!(err.contains("waive"), "{err}");
}

#[test]
fn a_project_cannot_waive_probation() {
    let err = parse_as(
        "unattended:\n  waive_probation: true\n",
        PolicySource::Project,
    )
    .unwrap_err();
    assert!(
        err.contains(
            "unattended.waive_probation is a machine setting; a project cannot waive probation"
        ),
        "{err}"
    );
    // The machine file sets it.
    let file = parse("unattended:\n  waive_probation: true\n").unwrap();
    assert!(file.unattended.waive_probation);
}

#[test]
fn a_project_only_raises_the_probation_floor() {
    let policies = |m: &str, p: &str| Policies {
        machine: parse(m).unwrap(),
        project: parse_as(p, PolicySource::Project).unwrap(),
    };
    assert_eq!(policies("", "").probation_floor(), None);
    assert_eq!(
        policies("unattended:\n  after_attended_successes: 3\n", "").probation_floor(),
        Some((3, PolicySource::Machine))
    );
    assert_eq!(
        policies("", "unattended:\n  after_attended_successes: 5\n").probation_floor(),
        Some((5, PolicySource::Project))
    );
    // A project raises, never lowers.
    assert_eq!(
        policies(
            "unattended:\n  after_attended_successes: 5\n",
            "unattended:\n  after_attended_successes: 3\n"
        )
        .probation_floor(),
        Some((5, PolicySource::Machine))
    );
    // The machine's waive flag is the only one read.
    assert!(
        policies("unattended:\n  waive_probation: true\n", "").waives_probation(),
        "the machine file waives"
    );
    assert!(
        !policies("", "unattended:\n  after_attended_successes: 2\n").waives_probation(),
        "a project file sets no waiver"
    );
}

// --- reminders for open waits -------------------------------------------------

/// The `remind` entries are durations, 10m to 7d, at most five, strictly
/// increasing; an empty list is valid. Each error names its value.
#[test]
fn remind_entries_are_checked() {
    let err = parse("notify:\n  remind: [5m]\n").unwrap_err();
    assert!(err.contains("`5m` is shorter than 10m"), "{err}");
    let err = parse("notify:\n  remind: [8d]\n").unwrap_err();
    assert!(err.contains("`8d` is longer than 7d"), "{err}");
    let err = parse("notify:\n  remind: [1h, 30m]\n").unwrap_err();
    assert!(err.contains("`30m` does not come after `1h`"), "{err}");
    let err = parse("notify:\n  remind: [1h, 1h]\n").unwrap_err();
    assert!(err.contains("`1h` does not come after `1h`"), "{err}");
    let err = parse("notify:\n  remind: [soon]\n").unwrap_err();
    assert!(err.contains("`soon` is not a duration"), "{err}");
    let six = "notify:\n  remind: [10m, 1h, 4h, 24h, 3d, 7d]\n";
    let err = parse(six).unwrap_err();
    assert!(err.contains("6 entries; use at most 5"), "{err}");
    let ok = parse("notify:\n  remind: [10m, 1h, 4h, 24h, 7d]\n").unwrap();
    assert_eq!(
        ok.notify.remind.as_deref(),
        Some(
            ["10m", "1h", "4h", "24h", "7d"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
                .as_slice()
        )
    );
    // An empty list is valid: no reminders.
    assert!(parse("notify:\n  remind: []\n").is_ok());
    // A quoted bare number is a duration of seconds.
    assert!(parse("notify:\n  remind: [\"3600\"]\n").is_ok());
}

/// A project file may only turn reminders off: any other value is refused.
#[test]
fn a_project_may_only_turn_reminders_off() {
    let _sandbox = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let project = project_path(root.path());
    std::fs::write(&project, "notify:\n  remind: [5m]\n").unwrap();
    let err = Policies::load(root.path()).unwrap_err();
    assert!(
        err.message
            .contains("a project may only turn reminders off"),
        "{err}"
    );
    // `remind: []` is fine.
    std::fs::write(&project, "notify:\n  remind: []\n").unwrap();
    assert!(Policies::load(root.path()).is_ok());
}

/// The reminders in force are the machine's list in milliseconds, emptied
/// by the project's `remind: []`.
#[test]
fn remind_ms_reads_the_machine_list() {
    let p = policies("notify:\n  remind: [1h30m, 4h]\n", "");
    assert_eq!(p.remind_ms(), vec![5_400_000, 14_400_000]);
    let p = policies("notify:\n  remind: [1h]\n", "notify:\n  remind: []\n");
    assert_eq!(p.remind_ms(), Vec::<u64>::new());
    let p = policies("", "notify:\n  remind: []\n");
    assert_eq!(p.remind_ms(), Vec::<u64>::new());
}
