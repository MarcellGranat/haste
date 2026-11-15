//! Daemon + Reminders connector against a fake helper: first-install kill switch, sync,
//! create-from-thread, complete, ambiguous outcomes and permission loss. No EventKit involved.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use haste_connector_reminders::{
    AuthStatus, CycleAudit, Helper, HelperError, RemindersConnector, RemindersRequest,
    RemindersResponse, WireReminder,
};
use haste_core::connector::{CallError, Connector};
use haste_core::model::Source;
use haste_core::policy::PrincipalKind;
use haste_core::proposal::{Payload, ProposalState};
use haste_proto::{encode_frame, parse_header, ErrorCode, Request, RequestBody, Response};
use hasted::notifier::RecordingNotifier;
use hasted::scheduler::sync_once;
use hasted::{start, start_reminders_with, DaemonConfig, DaemonHandle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    TimeOutOnWrite,
    NoPermission,
}

struct FakeHelper {
    reminders: Mutex<Vec<WireReminder>>,
    mode: Mutex<Mode>,
    calls: Mutex<Vec<RemindersRequest>>,
    next: Mutex<u32>,
}

fn wire(id: &str, title: &str) -> WireReminder {
    WireReminder {
        external_id: id.into(),
        title: title.into(),
        notes: "SECRET NOTES".into(),
        due: None,
        completed: false,
        list: "SECRET LIST".into(),
        modified: 10,
        repeat: None,
    }
}

impl Helper for FakeHelper {
    fn call(&self, request: &RemindersRequest) -> Result<RemindersResponse, HelperError> {
        self.calls.lock().unwrap().push(request.clone());
        let mode = *self.mode.lock().unwrap();
        if let RemindersRequest::Authorization { .. } = request {
            return Ok(RemindersResponse::Authorization {
                status: if mode == Mode::NoPermission {
                    AuthStatus::Denied
                } else {
                    AuthStatus::Granted
                },
            });
        }
        if mode == Mode::NoPermission {
            return Ok(RemindersResponse::Error {
                kind: "not_authorized".into(),
                message: String::new(),
            });
        }
        let mut list = self.reminders.lock().unwrap();
        match request {
            RemindersRequest::FetchChanged { .. } => Ok(RemindersResponse::FetchChanged {
                reminders: list.clone(),
                has_more: false,
            }),
            RemindersRequest::CreateReminder {
                title,
                notes,
                repeat,
                due,
                ..
            } => {
                if mode == Mode::TimeOutOnWrite {
                    // the write may or may not have happened
                    return Err(HelperError::TimedOut);
                }
                let mut n = self.next.lock().unwrap();
                *n += 1;
                let mut r = wire(&format!("new-{n}"), title);
                r.notes = notes.clone().unwrap_or_default();
                r.repeat = *repeat;
                r.due = *due;
                list.push(r.clone());
                Ok(RemindersResponse::CreateReminder { reminder: r })
            }
            RemindersRequest::CompleteReminder { external_id } => {
                match list.iter_mut().find(|r| &r.external_id == external_id) {
                    Some(r) if !r.completed => {
                        r.completed = true;
                        Ok(RemindersResponse::CompleteReminder {})
                    }
                    Some(_) => Ok(RemindersResponse::Error {
                        kind: "already_completed".into(),
                        message: String::new(),
                    }),
                    None => Ok(RemindersResponse::Error {
                        kind: "not_found".into(),
                        message: String::new(),
                    }),
                }
            }
            RemindersRequest::UpdateReminder {
                external_id,
                title,
                append_notes,
                repeat,
                set_repeat,
                due,
                ..
            } => match list.iter_mut().find(|r| &r.external_id == external_id) {
                Some(r) => {
                    r.title = title.clone();
                    r.due = *due;
                    if *set_repeat {
                        r.repeat = *repeat;
                    }
                    if let Some(extra) = append_notes {
                        r.notes = format!("{}{extra}", r.notes);
                    }
                    Ok(RemindersResponse::UpdateReminder {})
                }
                None => Ok(RemindersResponse::Error {
                    kind: "not_found".into(),
                    message: String::new(),
                }),
            },
            RemindersRequest::ListLists => Ok(RemindersResponse::ListLists { count: 1 }),
            RemindersRequest::Authorization { .. } => unreachable!(),
        }
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    daemon: DaemonHandle,
    helper: Arc<FakeHelper>,
    connector: Arc<dyn Connector>,
    human: String,
}

async fn boot() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mut config = DaemonConfig::new(
        &dir.path().join("haste"),
        Arc::new(RecordingNotifier::default()),
    );
    config.expiry_tick = Duration::from_secs(3600);
    config.delivery_tick = Duration::from_secs(3600);
    let daemon = start(config).await.unwrap();
    let human = fs::read_to_string(daemon.state.config.human_token_path()).unwrap();
    let helper = Arc::new(FakeHelper {
        reminders: Mutex::new(vec![wire("ek-1", "SECRET TITLE one"), wire("ek-2", "two")]),
        mode: Mutex::new(Mode::Normal),
        calls: Mutex::new(Vec::new()),
        next: Mutex::new(0),
    });
    start_reminders_with(&daemon.state, helper.clone(), 3600);
    let state = Arc::clone(&daemon.state);
    let audit: CycleAudit = Arc::new(move |o, d| {
        state.audit("connector", "reminders.sync", o, Some("reminders"), Some(d));
    });
    let connector: Arc<dyn Connector> = Arc::new(RemindersConnector::new(helper.clone(), audit));
    daemon
        .state
        .store
        .replace_machine_labels(
            "m1",
            &std::collections::BTreeSet::from(["haste".to_owned()]),
        )
        .unwrap();
    Rig {
        _dir: dir,
        daemon,
        helper,
        connector,
        human,
    }
}

async fn call(r: &Rig, token: &str, body: RequestBody) -> Response {
    let mut stream = UnixStream::connect(&r.daemon.socket_path).await.unwrap();
    stream
        .write_all(&encode_frame(&Request::new(token, body)).unwrap())
        .await
        .unwrap();
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).await.unwrap();
    let mut buf = vec![0_u8; parse_header(header).unwrap()];
    stream.read_exact(&mut buf).await.unwrap();
    serde_json::from_slice(&buf).unwrap()
}

async fn human(r: &Rig, body: RequestBody) -> Response {
    call(r, &r.human, body).await
}

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { error } => Some(error.code),
        _ => None,
    }
}

async fn agent(r: &Rig) -> String {
    let Response::Registered { token, .. } = human(
        r,
        RequestBody::RegisterPrincipal {
            principal_id: "agent-a".into(),
            kind: PrincipalKind::Agent,
            allowed_actions: vec![
                "read".into(),
                "reminder.create".into(),
                "reminder.complete".into(),
            ],
        },
    )
    .await
    else {
        panic!("register failed")
    };
    token
}

async fn release_and_sync(r: &Rig) {
    human(
        r,
        RequestBody::ConnectorRelease {
            source: "reminders".into(),
        },
    )
    .await;
    sync_once(&r.daemon.state, &r.connector).await.unwrap();
}

async fn propose(
    r: &Rig,
    token: &str,
    payload: Payload,
    thread: Option<String>,
    key: &str,
) -> Response {
    call(
        r,
        token,
        RequestBody::Propose {
            payload,
            thread_id: thread,
            idempotency_key: key.into(),
            hints: None,
        },
    )
    .await
}

fn proposal_of(r: Response) -> haste_core::Proposal {
    match r {
        Response::Proposal { proposal } => proposal,
        other => panic!("{other:?}"),
    }
}

fn create_payload(title: &str, project: Option<&str>) -> Payload {
    Payload::ReminderCreate {
        title: title.into(),
        due: None,
        list: None,
        project: project.map(str::to_owned),
        thread_id: None,
    }
}

#[tokio::test]
async fn a_fresh_install_is_engaged_and_makes_no_call_until_released() {
    let rig = boot().await;
    let first = sync_once(&rig.daemon.state, &rig.connector).await;
    assert_eq!(first.unwrap_err(), CallError::KillSwitchEngaged);
    assert!(
        rig.helper.calls.lock().unwrap().is_empty(),
        "no helper call"
    );
    release_and_sync(&rig).await;
    let Response::Reminders { reminders } = human(
        &rig,
        RequestBody::RemindersList {
            include_completed: true,
        },
    )
    .await
    else {
        panic!()
    };
    assert_eq!(reminders.len(), 2);
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn create_from_a_thread_then_complete_both_need_approval_and_execute() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let thread = rig
        .daemon
        .state
        .store
        .upsert_thread(&Source::Gmail, "g1", "SECRET SUBJECT", &[], 1)
        .unwrap()
        .id;
    let token = agent(&rig).await;

    // unknown project is refused at proposal time
    let bad = propose(&rig, &token, create_payload("x", Some("nope")), None, "k0").await;
    assert_eq!(code(&bad), Some(ErrorCode::Validation));

    let p = proposal_of(
        propose(
            &rig,
            &token,
            create_payload("Call Ana", Some("haste")),
            Some(thread.clone()),
            "k1",
        )
        .await,
    );
    assert_eq!(p.state, ProposalState::Pending);
    // an agent can neither approve nor execute
    for body in [
        RequestBody::Approve {
            proposal_id: p.id.clone(),
            acknowledge_stale: true,
        },
        RequestBody::Execute {
            proposal_id: p.id.clone(),
            acknowledge_stale: true,
        },
    ] {
        assert_eq!(
            code(&call(&rig, &token, body).await),
            Some(ErrorCode::Denied)
        );
    }
    // a pending proposal cannot be executed even by the human
    let early = human(
        &rig,
        RequestBody::Execute {
            proposal_id: p.id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    assert_eq!(code(&early), Some(ErrorCode::IllegalTransition));
    human(
        &rig,
        RequestBody::Approve {
            proposal_id: p.id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    let done = proposal_of(
        human(
            &rig,
            RequestBody::Execute {
                proposal_id: p.id.clone(),
                acknowledge_stale: false,
            },
        )
        .await,
    );
    assert_eq!(done.state, ProposalState::Sent);
    // only an identifier reached Reminders
    let calls = rig.helper.calls.lock().unwrap().clone();
    let notes = calls
        .iter()
        .find_map(|c| match c {
            RemindersRequest::CreateReminder { notes, .. } => notes.clone(),
            _ => None,
        })
        .unwrap();
    assert_eq!(notes, format!("haste:thread/{thread}"));
    let store = &rig.daemon.state.store;
    assert_eq!(store.count_links().unwrap(), 1);
    let created = store
        .list_reminders(true)
        .unwrap()
        .into_iter()
        .find(|r| r.external_id == "new-1")
        .unwrap();
    assert_eq!(store.reminder_tags(&created.id).unwrap(), ["haste"]);

    // complete the synced reminder
    let target = store
        .list_reminders(false)
        .unwrap()
        .into_iter()
        .find(|r| r.external_id == "ek-2")
        .unwrap();
    let c = proposal_of(
        propose(
            &rig,
            &token,
            Payload::ReminderComplete {
                reminder_id: target.id.clone(),
                project: None,
                reminder_version: None,
            },
            None,
            "k2",
        )
        .await,
    );
    human(
        &rig,
        RequestBody::Approve {
            proposal_id: c.id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    let done = proposal_of(
        human(
            &rig,
            RequestBody::Execute {
                proposal_id: c.id.clone(),
                acknowledge_stale: false,
            },
        )
        .await,
    );
    assert_eq!(done.state, ProposalState::Sent);
    assert!(store.get_reminder(&target.id).unwrap().unwrap().completed);

    let audit = fs::read_to_string(rig.daemon.state.config.audit_path()).unwrap();
    for secret in ["SECRET", "Call Ana"] {
        assert!(!audit.contains(secret), "audit leaked {secret}");
    }
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn an_unknown_outcome_stays_executing_and_a_lost_permission_returns_to_approved() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let token = agent(&rig).await;
    let p = proposal_of(propose(&rig, &token, create_payload("t", None), None, "k1").await);
    human(
        &rig,
        RequestBody::Approve {
            proposal_id: p.id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;

    // the helper times out: the write may have happened, so it is NOT retried automatically
    *rig.helper.mode.lock().unwrap() = Mode::TimeOutOnWrite;
    let exec = RequestBody::Execute {
        proposal_id: p.id.clone(),
        acknowledge_stale: false,
    };
    assert_eq!(
        code(&human(&rig, exec.clone()).await),
        Some(ErrorCode::Internal)
    );
    let again = human(&rig, exec.clone()).await;
    assert_eq!(
        code(&again),
        Some(ErrorCode::IllegalTransition),
        "an executing proposal is never re-run"
    );
    let Response::RemindersStatus { status } = human(&rig, RequestBody::RemindersStatus).await
    else {
        panic!()
    };
    assert_eq!(status.needs_check, 1);
    assert_eq!(status.permission, "granted");
    assert!(status.total >= 2);

    // the human looked in Reminders and found nothing: back to approved
    let back = proposal_of(
        human(
            &rig,
            RequestBody::ResolveExecuting {
                proposal_id: p.id.clone(),
                done: false,
            },
        )
        .await,
    );
    assert_eq!(back.state, ProposalState::Approved);

    // permission lost: certainly nothing happened, so it returns to approved with a reason
    *rig.helper.mode.lock().unwrap() = Mode::NoPermission;
    assert_eq!(code(&human(&rig, exec).await), Some(ErrorCode::Denied));
    let status = proposal_of(
        human(
            &rig,
            RequestBody::ProposalStatus {
                proposal_id: p.id.clone(),
            },
        )
        .await,
    );
    assert_eq!(status.state, ProposalState::Approved);
    assert_eq!(status.result.as_deref(), Some("permission needed"));
    // and the next sync reports it without tripping the breaker
    assert!(sync_once(&rig.daemon.state, &rig.connector).await.is_err());
    let Response::Connectors { connectors } = human(&rig, RequestBody::ConnectorStatus).await
    else {
        panic!()
    };
    assert!(connectors
        .iter()
        .any(|c| c.source == "reminders" && !c.engaged));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_restart_leaves_an_executing_proposal_untouched_and_reports_it() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let token = agent(&rig).await;
    let p = proposal_of(propose(&rig, &token, create_payload("t", None), None, "k1").await);
    human(
        &rig,
        RequestBody::Approve {
            proposal_id: p.id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    rig.daemon
        .state
        .store
        .begin_execution(&p.id, false, 5)
        .unwrap();
    assert_eq!(hasted::report_interrupted_executions(&rig.daemon.state), 1);
    let after = rig.daemon.state.store.get_proposal(&p.id).unwrap();
    assert_eq!(after.state, ProposalState::Executing);
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_new_reminder_carries_its_repeat_rule_thread_link_and_shows_at_once() {
    use haste_core::model::{RepeatFreq, RepeatRule};
    let rig = boot().await;
    release_and_sync(&rig).await;
    let thread = rig
        .daemon
        .state
        .store
        .upsert_thread(
            &haste_core::model::Source::Gmail,
            "g-r1",
            "Rent",
            &["landlord@x.org".into()],
            5,
        )
        .unwrap();
    let rule = RepeatRule {
        freq: RepeatFreq::Month,
        every: 2,
    };
    assert!(matches!(
        human(
            &rig,
            RequestBody::ReminderNew {
                title: "Pay rent".into(),
                due: Some(1_800_000_000),
                notes: Some("haste:thread/x".into()),
                repeat: Some(rule),
                tags: vec![],
                thread_id: Some(thread.id.clone()),
            }
        )
        .await,
        Response::Done
    ));
    // the helper was asked to repeat it
    assert!(rig.helper.calls.lock().unwrap().iter().any(|c| matches!(
        c,
        RemindersRequest::CreateReminder { repeat: Some(r), .. } if *r == rule
    )));
    // and the list shows it with its rule, before the next sync
    let Response::Reminders { reminders } = human(
        &rig,
        RequestBody::RemindersList {
            include_completed: false,
        },
    )
    .await
    else {
        panic!()
    };
    let made = reminders
        .iter()
        .find(|r| r.reminder.title == "Pay rent")
        .expect("listed at once");
    assert_eq!(made.repeat, Some(rule));
    assert_eq!(made.reminder.due_at, Some(1_800_000_000));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn labels_set_in_the_popup_are_saved_for_a_new_todo_and_for_an_edit() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    rig.daemon
        .state
        .store
        .replace_machine_labels(
            "local",
            &["haste".to_owned(), "work".to_owned()]
                .into_iter()
                .collect(),
        )
        .unwrap();
    // a new TODO with a label
    assert!(matches!(
        human(
            &rig,
            RequestBody::ReminderNew {
                title: "labelled".into(),
                due: None,
                notes: None,
                repeat: None,
                tags: vec!["work".into()],
                thread_id: None,
            }
        )
        .await,
        Response::Done
    ));
    let list = |rig: &Rig| rig.daemon.state.store.list_reminders(false).unwrap();
    let all = rig.daemon.state.store.all_reminder_tags().unwrap();
    let made = list(&rig)
        .into_iter()
        .find(|r| r.title == "labelled")
        .expect("created");
    assert_eq!(all.get(&made.id), Some(&vec!["work".to_owned()]));
    // an edit adds one and takes one away
    for (tag, add) in [("haste", true), ("work", false)] {
        let request = if add {
            RequestBody::ReminderTagAdd {
                reminder_id: made.id.clone(),
                tag: tag.into(),
            }
        } else {
            RequestBody::ReminderTagRemove {
                reminder_id: made.id.clone(),
                tag: tag.into(),
            }
        };
        assert!(matches!(human(&rig, request).await, Response::Done));
    }
    let all = rig.daemon.state.store.all_reminder_tags().unwrap();
    assert_eq!(all.get(&made.id), Some(&vec!["haste".to_owned()]));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn editing_a_reminder_can_set_and_clear_how_it_repeats() {
    use haste_core::model::{RepeatFreq, RepeatRule};
    let rig = boot().await;
    release_and_sync(&rig).await;
    let list = |rig: &Rig| {
        let r = rig.daemon.state.store.list_reminders(false).unwrap();
        (r[0].id.clone(), r[0].external_id.clone())
    };
    let (id, external) = list(&rig);
    let rule = RepeatRule {
        freq: RepeatFreq::Week,
        every: 2,
    };
    let edit = |repeat, set_repeat| RequestBody::ReminderEdit {
        reminder_id: id.clone(),
        title: "same".into(),
        due: Some(1_800_000_000),
        append_notes: None,
        repeat,
        set_repeat,
    };
    assert!(matches!(
        human(&rig, edit(Some(rule), true)).await,
        Response::Done
    ));
    assert!(rig.helper.calls.lock().unwrap().iter().any(|c| matches!(
        c,
        RemindersRequest::UpdateReminder { repeat: Some(r), set_repeat: true, .. } if *r == rule
    )));
    // an edit that does not mention the repeat tells the helper to leave it alone
    human(&rig, edit(None, false)).await;
    // and setting it to none tells the helper to stop it
    human(&rig, edit(None, true)).await;
    let calls = rig.helper.calls.lock().unwrap().clone();
    let updates: Vec<(Option<RepeatRule>, bool)> = calls
        .iter()
        .filter_map(|c| match c {
            RemindersRequest::UpdateReminder {
                repeat, set_repeat, ..
            } => Some((*repeat, *set_repeat)),
            _ => None,
        })
        .collect();
    assert_eq!(
        updates,
        vec![(Some(rule), true), (None, false), (None, true)]
    );
    let _ = external;
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn the_human_creates_and_edits_reminders_directly_and_an_agent_cannot() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let Response::Reminders { reminders } = human(
        &rig,
        RequestBody::RemindersList {
            include_completed: true,
        },
    )
    .await
    else {
        panic!()
    };
    let id = reminders[0].reminder.id.clone();

    // no approval step: the human's edit and new TODO run at once
    let edit = RequestBody::ReminderEdit {
        reminder_id: id.clone(),
        title: "renamed".into(),
        due: Some(1_800_000_000),
        append_notes: Some("see the attached file".into()),
        repeat: None,
        set_repeat: false,
    };
    assert!(matches!(human(&rig, edit.clone()).await, Response::Done));
    assert!(matches!(
        human(
            &rig,
            RequestBody::ReminderNew {
                title: "fresh".into(),
                due: None,
                notes: Some("n".into()),
                repeat: None,
                tags: vec![],
                thread_id: None,
            }
        )
        .await,
        Response::Done
    ));
    {
        let list = rig.helper.reminders.lock().unwrap();
        let edited = list.iter().find(|r| r.title == "renamed").unwrap();
        assert!(edited.notes.ends_with("see the attached file"));
        assert!(
            edited.notes.starts_with("SECRET NOTES"),
            "old notes are kept"
        );
        assert!(list.iter().any(|r| r.title == "fresh"));
    }
    // an empty title is refused, and an agent may do neither
    assert_eq!(
        code(
            &human(
                &rig,
                RequestBody::ReminderNew {
                    title: "  ".into(),
                    due: None,
                    notes: None,
                    repeat: None,
                    tags: vec![],
                    thread_id: None,
                }
            )
            .await
        ),
        Some(ErrorCode::Validation)
    );
    let token = agent(&rig).await;
    assert_eq!(
        code(&call(&rig, &token, edit).await),
        Some(ErrorCode::Denied)
    );
    rig.daemon.shutdown().await;
}
