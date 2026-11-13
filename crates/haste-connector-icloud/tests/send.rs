//! A human send through the iCloud connector against loopback fakes: SMTP for the submission and
//! IMAP for the Sent copy. No network, no real Keychain.

use std::sync::Arc;

use haste_connector_icloud::config::IcloudConfig;
use haste_connector_icloud::secrets::{MemoryStore, SecretStore};
use haste_connector_icloud::session::PlainOpener;
use haste_connector_icloud::smtp::PlainSmtp;
use haste_connector_icloud::testing::{
    spawn, spawn_smtp, FakeFolder, FakeImap, FakeSmtp, FakeState, SmtpFault,
};
use haste_connector_icloud::{CycleAudit, IcloudConnector};
use haste_core::connector::{Connector, ConnectorError, DraftToSend, Guarded, Hooks};
use haste_core::model::Source;

const PASSWORD: &str = "hunter2-app-password";
const ADDRESS: &str = "me@icloud.com";
const RECIPIENT: &str = "ana@example.org";

struct Rig {
    connector: IcloudConnector,
    smtp: FakeSmtp,
    imap: FakeImap,
}

fn rig(fault: SmtpFault, tweak: impl FnOnce(&mut IcloudConfig, &mut FakeState)) -> Rig {
    let mut state = FakeState {
        password: PASSWORD.into(),
        ..FakeState::default()
    };
    state
        .folders
        .insert("Sent Messages".into(), FakeFolder::default());
    let smtp = spawn_smtp(ADDRESS, PASSWORD, fault);
    let mut config = IcloudConfig::new(ADDRESS);
    config.host = "127.0.0.1".into();
    config.smtp_host = "127.0.0.1".into();
    config.smtp_port = smtp.port;
    tweak(&mut config, &mut state);
    let imap = spawn(state);
    config.port = imap.port;
    let secrets = Arc::new(MemoryStore::default());
    secrets.set(ADDRESS, PASSWORD).unwrap();
    let audit: CycleAudit = Arc::new(|_, _| {});
    let connector = IcloudConnector::new(config, secrets, Arc::new(PlainOpener), audit).with_smtp(
        Arc::new(PlainSmtp {
            timeout: std::time::Duration::from_millis(700),
        }),
    );
    Rig {
        connector,
        smtp,
        imap,
    }
}

fn draft() -> DraftToSend {
    DraftToSend {
        attachments: Vec::new(),
        thread_id: None,
        to: Some(RECIPIENT.into()),
        body: "Noon is great.\n.leading dot line".into(),
        recipients: vec![RECIPIENT.into()],
        subject: Some("Re: Lunch".into()),
        approval_draft_id: None,
    }
}

fn smtp_commands(r: &Rig) -> Vec<String> {
    r.smtp.log.lock().unwrap().commands.clone()
}

fn appended(r: &Rig) -> Vec<(String, String)> {
    r.imap.state.lock().unwrap().appended.clone()
}

#[tokio::test]
async fn a_send_submits_once_in_the_right_order_and_files_exactly_one_copy_afterwards() {
    let r = rig(SmtpFault::None, |_, _| {});
    let receipt = r.connector.send(&draft()).await.unwrap();
    assert_eq!(receipt.external_id, "smtp");
    assert_eq!(
        smtp_commands(&r),
        [
            "EHLO haste.local",
            "AUTH PLAIN",
            "MAIL FROM:<me@icloud.com>",
            "RCPT TO:<ana@example.org>",
            "DATA",
            "QUIT"
        ]
    );
    let log = r.smtp.log.lock().unwrap();
    assert_eq!(log.connections, 1, "one submission");
    assert_eq!(log.messages.len(), 1);
    let msg = &log.messages[0];
    for h in [
        "From: me@icloud.com",
        "To: ana@example.org",
        "Subject: Re: Lunch",
        "Message-ID: <",
    ] {
        assert!(msg.contains(h), "{h}");
    }
    assert!(!msg.to_lowercase().contains("bcc"));
    assert!(
        msg.ends_with("Noon is great.\r\n.leading dot line\r\n"),
        "dot-stuffing undone: {msg:?}"
    );
    drop(log);
    // the Sent copy: the same bytes, once, after acceptance, in the configured folder
    let copies = appended(&r);
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].0, "Sent Messages");
    assert!(copies[0].1.contains("Subject: Re: Lunch"));
    assert_eq!(r.smtp.log.lock().unwrap().messages[0], copies[0].1);
}

#[tokio::test]
async fn the_sent_copy_can_be_turned_off_and_its_folder_chosen() {
    let r = rig(SmtpFault::None, |c, _| c.append_sent = false);
    assert_eq!(
        r.connector.send(&draft()).await.unwrap().external_id,
        "smtp"
    );
    assert!(appended(&r).is_empty());
    assert!(r
        .imap
        .state
        .lock()
        .unwrap()
        .commands
        .iter()
        .all(|c| !c.contains("APPEND")));
    let r = rig(SmtpFault::None, |c, s| {
        c.sent_folder = "Sent".into();
        s.folders.insert("Sent".into(), FakeFolder::default());
    });
    r.connector.send(&draft()).await.unwrap();
    assert_eq!(appended(&r)[0].0, "Sent");
}

#[tokio::test]
async fn a_failed_copy_keeps_the_send_and_never_submits_again() {
    let r = rig(SmtpFault::None, |_, s| s.append_fails = true);
    let receipt = r.connector.send(&draft()).await.unwrap();
    assert_eq!(receipt.external_id, "smtp;copy_failed");
    assert_eq!(
        r.smtp.log.lock().unwrap().messages.len(),
        1,
        "submitted once, not retried"
    );
    assert_eq!(r.smtp.log.lock().unwrap().connections, 1);
}

#[tokio::test]
async fn every_refusal_before_the_body_delivers_nothing_files_no_copy_and_does_not_retry() {
    for (fault, expect) in [
        (SmtpFault::GreetingRejected, "Transient"),
        (SmtpFault::MailFromRefused, "refused"),
        (SmtpFault::RcptToRefused, "refused"),
        (SmtpFault::DataRefused, "refused"),
        (SmtpFault::AuthRejected, "Checkpoint"),
    ] {
        let r = rig(fault, |_, _| {});
        let err = r.connector.send(&draft()).await.unwrap_err();
        match (&err, expect) {
            (ConnectorError::Transient(_), "Transient")
            | (ConnectorError::Checkpoint(_), "Checkpoint") => {}
            (ConnectorError::Other(m), "refused") => assert!(m.starts_with("send refused"), "{m}"),
            other => panic!("{fault:?}: {other:?}"),
        }
        let log = r.smtp.log.lock().unwrap();
        assert_eq!(
            log.connections, 1,
            "{fault:?}: a single connection, no retry"
        );
        assert!(log.messages.is_empty(), "{fault:?}: nothing was delivered");
        let c = &log.commands;
        for verb in ["MAIL FROM", "RCPT TO", "DATA", "AUTH PLAIN"] {
            assert!(
                c.iter().filter(|l| l.starts_with(verb)).count() <= 1,
                "{fault:?}: {verb} repeated"
            );
        }
        drop(log);
        assert!(
            appended(&r).is_empty(),
            "{fault:?}: no copy before acceptance"
        );
    }
}

#[tokio::test]
async fn anything_after_the_body_is_ambiguous_and_files_no_copy() {
    for fault in [
        SmtpFault::DropAfterBody,
        SmtpFault::FinalReplyRejected,
        SmtpFault::HangAfterBody,
    ] {
        let r = rig(fault, |_, _| {});
        let err = r.connector.send(&draft()).await.unwrap_err();
        assert!(
            matches!(err, ConnectorError::Ambiguous(_)),
            "{fault:?}: {err:?}"
        );
        assert_eq!(
            r.smtp.log.lock().unwrap().connections,
            1,
            "{fault:?}: never re-submitted"
        );
        assert!(
            appended(&r).is_empty(),
            "{fault:?}: no copy for a message we are unsure of"
        );
    }
}

#[tokio::test]
async fn a_refused_header_or_missing_recipient_never_connects() {
    let r = rig(SmtpFault::None, |_, _| {});
    for bad in ["a@x.org\r\nBcc: e@x.org", "a@x.org\nX: y", "a@x.org\0"] {
        let mut d = draft();
        d.recipients = vec![bad.into()];
        let err = r.connector.send(&d).await.unwrap_err();
        assert_eq!(
            err,
            ConnectorError::Other("send refused: header characters".into()),
            "{bad:?}"
        );
    }
    let mut no_recipient = draft();
    no_recipient.recipients.clear();
    assert!(
        matches!(r.connector.send(&no_recipient).await, Err(ConnectorError::Other(m)) if m.starts_with("send refused"))
    );
    let mut injected_subject = draft();
    injected_subject.subject = Some("hi\r\nBcc: e@x.org".into());
    assert!(r.connector.send(&injected_subject).await.is_err());
    assert_eq!(
        r.smtp.log.lock().unwrap().connections,
        0,
        "nothing was even connected"
    );
}

#[tokio::test]
async fn a_missing_password_is_a_checkpoint_and_a_dead_server_a_connection_failure() {
    let r = rig(SmtpFault::None, |_, _| {});
    let empty = Arc::new(MemoryStore::default());
    let audit: CycleAudit = Arc::new(|_, _| {});
    let no_password = IcloudConnector::new(
        IcloudConfig::new(ADDRESS),
        empty,
        Arc::new(PlainOpener),
        audit,
    );
    assert!(matches!(
        no_password.send(&draft()).await,
        Err(ConnectorError::Checkpoint(_))
    ));
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let secrets = Arc::new(MemoryStore::default());
    secrets.set(ADDRESS, PASSWORD).unwrap();
    let mut config = IcloudConfig::new(ADDRESS);
    config.smtp_host = "127.0.0.1".into();
    config.smtp_port = port;
    let dead = IcloudConnector::new(config, secrets, Arc::new(PlainOpener), Arc::new(|_, _| {}))
        .with_smtp(Arc::new(PlainSmtp::default()));
    assert!(matches!(
        dead.send(&draft()).await,
        Err(ConnectorError::Transient(_))
    ));
    drop(r);
}

#[tokio::test]
async fn credentials_never_reach_the_fake_servers_log_or_an_error() {
    let r = rig(SmtpFault::AuthRejected, |_, _| {});
    let err = r.connector.send(&draft()).await.unwrap_err();
    assert!(!format!("{err:?}").contains(PASSWORD));
    let log = r.smtp.log.lock().unwrap();
    assert!(log.commands.iter().all(|c| !c.contains(PASSWORD)));
    assert!(
        log.commands.contains(&"AUTH PLAIN".to_owned()),
        "the AUTH line is recorded without its argument"
    );
}

#[derive(Default)]
struct Hk(std::sync::Mutex<Vec<String>>);

impl Hooks for Hk {
    fn kill_switch_engaged(&self, _: &Source) -> bool {
        false
    }
    fn set_kill_switch(&self, _: &Source, engaged: bool) {
        self.0.lock().unwrap().push(format!("engaged={engaged}"));
    }
    fn audit(&self, action: &str, _: &Source, outcome: &str) {
        self.0.lock().unwrap().push(format!("{action}:{outcome}"));
    }
    fn notify(&self, _: &str) {}
}

#[tokio::test]
async fn through_the_guard_a_rejected_login_opens_the_breaker_and_an_ambiguous_send_does_not() {
    let bad_login = rig(SmtpFault::AuthRejected, |_, _| {});
    let hooks = Hk::default();
    let guard = Guarded {
        connector: &bad_login.connector,
        hooks: &hooks,
    };
    assert!(guard.send(&draft()).await.is_err());
    assert!(hooks.0.lock().unwrap().contains(&"engaged=true".to_owned()));
    let ambiguous = rig(SmtpFault::DropAfterBody, |_, _| {});
    let hooks = Hk::default();
    let guard = Guarded {
        connector: &ambiguous.connector,
        hooks: &hooks,
    };
    assert!(guard.send(&draft()).await.is_err());
    let events = hooks.0.lock().unwrap().clone();
    assert!(events.contains(&"send:ambiguous".to_owned()), "{events:?}");
    assert!(
        !events.iter().any(|e| e.starts_with("engaged=")),
        "{events:?}"
    );
}
