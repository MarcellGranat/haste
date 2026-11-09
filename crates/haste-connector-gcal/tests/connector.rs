//! The Calendar connector against a local fake Google server. No test calls Google.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use haste_connector_gcal::consts::{CLIENT_ACCOUNT, KEYCHAIN_SERVICE};
use haste_connector_gcal::{ClientCredentials, CycleAudit, GcalConfig, GcalConnector};
use haste_core::connector::{Connector, ConnectorError, Cursor, EventPatch, NewEvent};
use haste_google_oauth::keychain::{MemoryStore, TokenStore};
use haste_google_oauth::testing::{spawn, FakeServer, Recorded};
use serde_json::json;

const NOW: i64 = 1_794_218_400; // 2026-11-09T10:00:00Z
const ACCOUNT: &str = "me@example.org";
const TEAM: &str = "en.usa#holiday@group.v.calendar.google.com";

fn creds() -> ClientCredentials {
    ClientCredentials {
        client_id: "cid".into(),
        client_secret: "csecret".into(),
    }
}

fn config(calendars: &[&str]) -> GcalConfig {
    GcalConfig {
        account: ACCOUNT.into(),
        calendars: calendars.iter().map(|c| (*c).to_owned()).collect(),
    }
}

fn store() -> Arc<MemoryStore> {
    let s = MemoryStore::default();
    s.set(ACCOUNT, "refresh-1").unwrap();
    Arc::new(s)
}

fn audited() -> (CycleAudit, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    let audit: CycleAudit = Arc::new(move |o, d| sink.lock().unwrap().push(format!("{o}:{d}")));
    (audit, log)
}

fn connector(
    server: &FakeServer,
    cals: &[&str],
    s: Arc<dyn TokenStore>,
) -> (GcalConnector, Arc<Mutex<Vec<String>>>) {
    let (audit, log) = audited();
    let c = GcalConnector::new(
        &config(cals),
        &creds(),
        &format!("{}/calendar/v3", server.base),
        &format!("{}/token", server.base),
        s,
        audit,
    )
    .with_clock(Arc::new(|| NOW));
    (c, log)
}

fn ok(v: &serde_json::Value) -> (u16, String) {
    (200, v.to_string())
}

fn token_ok() -> (u16, String) {
    ok(&json!({"access_token":"at","expires_in":3600}))
}

fn timed(id: &str, title: &str, start: &str, end: &str) -> serde_json::Value {
    json!({"id":id,"status":"confirmed","summary":title,"start":{"dateTime":start},"end":{"dateTime":end},
           "updated":"2026-11-01T00:00:00Z","description":"SECRET DESCRIPTION","location":"SECRET PLACE",
           "attendees":[{"email":"secret.person@example.org"}]})
}

fn no_deletes(server: &FakeServer) {
    assert!(
        server.requests().iter().all(|r| r.method != "DELETE"),
        "the connector must never issue a DELETE"
    );
}

fn has_param(r: &Recorded, key: &str) -> bool {
    r.query.split('&').any(|p| p.split('=').next() == Some(key))
}

#[tokio::test]
async fn the_first_listing_has_no_time_window_stores_only_in_window_events_and_keeps_the_token() {
    let server = spawn(|r| {
        if r.path == "/token" {
            return token_ok();
        }
        assert!(r.path.starts_with("/calendar/v3/calendars/"), "{}", r.path);
        if has_param(r, "pageToken") {
            ok(&json!({"items":[{"id":"series","status":"confirmed","summary":"Old series",
                  "start":{"dateTime":"2020-01-01T09:00:00Z"},"end":{"dateTime":"2020-01-01T10:00:00Z"},
                  "recurrence":["RRULE:FREQ=WEEKLY"]}],"nextSyncToken":"T1"}))
        } else {
            ok(&json!({"items":[
                timed("near","Standup","2026-11-10T09:00:00Z","2026-11-10T09:30:00Z"),
                timed("old","Ancient","2023-01-01T09:00:00Z","2023-01-01T10:00:00Z"),
                timed("far","Distant","2028-01-01T09:00:00Z","2028-01-01T10:00:00Z")],
                "nextPageToken":"p2"}))
        }
    })
    .await;
    let (c, log) = connector(&server, &["primary", TEAM], store());
    let batch = c.fetch(None).await.unwrap();
    let titles: Vec<_> = batch.events.iter().map(|e| e.title.as_str()).collect();
    // two calendars, each lists the same fixture: near + the recurring master
    assert_eq!(titles.iter().filter(|t| **t == "Standup").count(), 2);
    assert_eq!(titles.iter().filter(|t| **t == "Old series").count(), 2);
    assert!(!titles.contains(&"Ancient") && !titles.contains(&"Distant"));
    let listing: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|r| r.path != "/token")
        .collect();
    assert!(listing.iter().all(|r| !has_param(r, "timeMin")
        && !has_param(r, "timeMax")
        && !has_param(r, "syncToken")));
    assert!(
        listing.iter().any(|r| r
            .path
            .contains("en.usa%23holiday@group.v.calendar.google.com")),
        "a # in a calendar id is percent-encoded"
    );
    assert_eq!(
        batch.calendar_sweeps.len(),
        2,
        "each full listing ends in a sweep"
    );
    let cursor =
        haste_connector_gcal::connector::CalendarCursor::decode(&batch.next_cursor.unwrap().0);
    assert_eq!(
        cursor.calendars.get("primary").map(String::as_str),
        Some("T1")
    );
    assert_eq!(cursor.calendars.len(), 2);
    assert_eq!(
        log.lock().unwrap().as_slice(),
        ["ok:calendars=2 events=4 full=2 resets=0"]
    );
    no_deletes(&server);
}

#[tokio::test]
async fn later_cycles_use_the_sync_token_and_carry_cancellations() {
    let server = spawn(|r| {
        if r.path == "/token" {
            return token_ok();
        }
        assert!(r.query.contains("syncToken=T1"), "{}", r.query);
        ok(&json!({"items":[{"id":"gone","status":"cancelled"},
             timed("changed","Moved","2026-11-11T09:00:00Z","2026-11-11T10:00:00Z")],"nextSyncToken":"T2"}))
    })
    .await;
    let (c, _) = connector(&server, &["primary"], store());
    let cursor = haste_connector_gcal::connector::CalendarCursor {
        calendars: [("primary".to_owned(), "T1".to_owned())].into(),
    };
    let batch = c.fetch(Some(Cursor(cursor.encode()))).await.unwrap();
    assert!(
        batch.calendar_sweeps.is_empty(),
        "a delta never marks anything missing"
    );
    assert_eq!(batch.events.len(), 2);
    assert_eq!(
        batch
            .events
            .iter()
            .find(|e| e.status == "cancelled")
            .unwrap()
            .external_id,
        "primary/gone"
    );
    let next =
        haste_connector_gcal::connector::CalendarCursor::decode(&batch.next_cursor.unwrap().0);
    assert_eq!(next.calendars["primary"], "T2");
}

#[tokio::test]
async fn a_gone_token_relists_once_and_a_second_gone_does_not_loop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let server = spawn(move |r| {
        if r.path == "/token" {
            return token_ok();
        }
        seen.fetch_add(1, Ordering::SeqCst);
        if has_param(r, "syncToken") {
            (410, json!({"error":{"code":410,"message":"gone"}}).to_string())
        } else {
            ok(&json!({"items":[timed("a","Standup","2026-11-10T09:00:00Z","2026-11-10T09:30:00Z")],"nextSyncToken":"T9"}))
        }
    })
    .await;
    let (c, log) = connector(&server, &["primary"], store());
    let cursor = haste_connector_gcal::connector::CalendarCursor {
        calendars: [("primary".to_owned(), "OLD".to_owned())].into(),
    };
    let batch = c.fetch(Some(Cursor(cursor.encode()))).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one 410, then exactly one full listing"
    );
    assert_eq!(batch.calendar_sweeps.len(), 1);
    assert!(log.lock().unwrap()[0].contains("resets=1"));

    // a full listing that is itself gone is an error, not a retry loop
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let server = spawn(move |r| {
        if r.path == "/token" {
            return token_ok();
        }
        seen.fetch_add(1, Ordering::SeqCst);
        (410, "{}".into())
    })
    .await;
    let (c, _) = connector(&server, &["primary"], store());
    assert!(c.fetch(Some(Cursor(cursor.encode()))).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn only_selected_calendars_are_requested_and_an_empty_list_reads_nothing() {
    let server = spawn(|r| {
        if r.path == "/token" {
            return token_ok();
        }
        assert!(
            r.path.contains("/calendars/primary/"),
            "unlisted calendar requested: {}",
            r.path
        );
        ok(&json!({"items":[],"nextSyncToken":"T"}))
    })
    .await;
    let (c, _) = connector(&server, &["primary"], store());
    c.fetch(None).await.unwrap();
    let empty = spawn(|_| panic!("no request may be made")).await;
    let (c, log) = connector(&empty, &[], store());
    let batch = c.fetch(None).await.unwrap();
    assert!(batch.events.is_empty());
    assert!(empty.requests().is_empty());
    assert_eq!(
        log.lock().unwrap().as_slice(),
        ["ok:calendars=0 events=0 full=0 resets=0"]
    );
}

#[tokio::test]
async fn writes_send_update_none_only_allow_listed_fields_and_never_delete() {
    let server = spawn(|r| {
        if r.path == "/token" {
            return token_ok();
        }
        ok(&timed(
            "new1",
            "Planning",
            "2026-11-12T09:00:00Z",
            "2026-11-12T10:00:00Z",
        ))
    })
    .await;
    let (c, _) = connector(&server, &["primary"], store());
    let receipt = c
        .create_event(&NewEvent {
            calendar: "primary".into(),
            title: "Planning".into(),
            start: NOW + 3 * 86_400,
            end: NOW + 3 * 86_400 + 3600,
            all_day: false,
            attendees: vec!["x@example.org".into()],
            description: None,
        })
        .await
        .unwrap();
    assert_eq!(receipt.event.external_id, "primary/new1");
    c.update_event(
        "primary/new1",
        &EventPatch {
            title: Some("Planning v2".into()),
            location: Some("Room 2".into()),
            ..EventPatch::default()
        },
    )
    .await
    .unwrap();
    let writes: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|r| r.path != "/token")
        .collect();
    assert_eq!(writes.len(), 2);
    for w in &writes {
        assert!(w.query.contains("sendUpdates=none"), "{}", w.query);
        assert!(!w.query.contains("sendUpdates=all") && !w.query.contains("externalOnly"));
    }
    assert_eq!(writes[0].method, "POST");
    assert_eq!(writes[1].method, "PATCH");
    assert!(writes[1].path.ends_with("/calendars/primary/events/new1"));
    let patch: serde_json::Value = serde_json::from_str(&writes[1].body).unwrap();
    let mut keys: Vec<_> = patch.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["location", "summary"], "no attendees, no status");
    no_deletes(&server);
}

#[tokio::test]
async fn a_write_to_an_unselected_calendar_or_an_empty_patch_makes_no_request() {
    let server = spawn(|_| panic!("no request may be made")).await;
    let (c, _) = connector(&server, &["primary"], store());
    let new = NewEvent {
        calendar: "other".into(),
        title: "t".into(),
        start: 1,
        end: 2,
        all_day: false,
        attendees: vec![],
        description: None,
    };
    assert!(matches!(
        c.create_event(&new).await,
        Err(ConnectorError::Unsupported(_))
    ));
    let patch = EventPatch {
        title: Some("x".into()),
        ..EventPatch::default()
    };
    assert!(matches!(
        c.update_event("other/e", &patch).await,
        Err(ConnectorError::Unsupported(_))
    ));
    assert!(matches!(
        c.update_event("primary/e", &EventPatch::default()).await,
        Err(ConnectorError::Unsupported(_))
    ));
    assert!(matches!(
        c.update_event("noslash", &patch).await,
        Err(ConnectorError::Unsupported(_))
    ));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn invalid_grant_is_a_checkpoint_so_the_breaker_opens() {
    let server = spawn(|r| {
        if r.path == "/token" {
            return (400, json!({"error":"invalid_grant"}).to_string());
        }
        panic!("no api call without a token");
    })
    .await;
    let (c, log) = connector(&server, &["primary"], store());
    assert!(matches!(
        c.fetch(None).await,
        Err(ConnectorError::Checkpoint(_))
    ));
    assert_eq!(log.lock().unwrap().as_slice(), ["auth_or_setup:"]);
}

struct Spy {
    inner: MemoryStore,
    reads: Mutex<Vec<String>>,
}

impl TokenStore for Spy {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        self.reads.lock().unwrap().push(account.to_owned());
        self.inner.get(account)
    }
    fn set(&self, account: &str, token: &str) -> Result<(), String> {
        self.inner.set(account, token)
    }
    fn delete(&self, account: &str) -> Result<(), String> {
        self.inner.delete(account)
    }
}

#[tokio::test]
async fn the_connector_reads_only_its_own_refresh_token_never_the_gmail_item() {
    assert_ne!(KEYCHAIN_SERVICE, haste_connector_gmail_service());
    let spy = Arc::new(Spy {
        inner: MemoryStore::default(),
        reads: Mutex::new(Vec::new()),
    });
    spy.set(ACCOUNT, "refresh-1").unwrap();
    let server = spawn(|r| {
        if r.path == "/token" {
            return token_ok();
        }
        ok(&json!({"items":[],"nextSyncToken":"T"}))
    })
    .await;
    let (c, _) = connector(&server, &["primary"], spy.clone());
    c.fetch(None).await.unwrap();
    // the refresh token (the address) is the only item read: not the OAuth client item
    assert_eq!(spy.reads.lock().unwrap().as_slice(), [ACCOUNT]);
    assert_ne!(CLIENT_ACCOUNT, ACCOUNT);
}

fn haste_connector_gmail_service() -> &'static str {
    "com.haste.gmail"
}

#[tokio::test]
async fn cycle_audit_carries_counts_only() {
    let server = spawn(|r| {
        if r.path == "/token" {
            return token_ok();
        }
        ok(&json!({"items":[timed("a","SECRET TITLE","2026-11-10T09:00:00Z","2026-11-10T09:30:00Z")],"nextSyncToken":"T"}))
    })
    .await;
    let (c, log) = connector(&server, &["primary"], store());
    c.fetch(None).await.unwrap();
    let joined = log.lock().unwrap().join("|");
    for secret in ["SECRET", "secret.person", "example.org"] {
        assert!(!joined.contains(secret), "audit leaked {secret}: {joined}");
    }
}

#[test]
fn the_connector_declares_read_and_the_calendar_write_flag_only_and_starts_engaged() {
    let (audit, _) = audited();
    let c = GcalConnector::new(
        &config(&["primary"]),
        &creds(),
        "http://127.0.0.1:1",
        "http://127.0.0.1:1/t",
        store(),
        audit,
    );
    let caps = c.capabilities();
    assert!(caps.read && caps.calendar_write);
    assert!(
        !caps.draft
            && !caps.send
            && !caps.archive
            && !caps.reminders_create
            && !caps.reminders_complete
    );
    assert!(c.default_engaged());
}
