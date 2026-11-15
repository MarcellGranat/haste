//! The Beeper connector against a loopback fake of Beeper Desktop's API. No live calls. The
//! shapes the fake serves are the provisional ones in `wire.rs`; the live spike confirms them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use haste_connector_beeper::client::BeeperClient;
use haste_connector_beeper::config::{AccountSelection, ChatSelection};
use haste_connector_beeper::connector::BeeperCursor;
use haste_connector_beeper::consts::{
    ATTACHMENT_LABEL, BEEPER_BACKFILL_PER_CHAT, BEEPER_BODY_MAX_BYTES, BEEPER_MAX_PAGES_PER_CHAT,
    BEEPER_MAX_SEND_BYTES, TOKEN_ACCOUNT,
};
use haste_connector_beeper::{BeeperConfig, BeeperConnector, BeeperSource, CycleAudit};
use haste_core::connector::{
    CallError, Connector, ConnectorError, Cursor, DraftToSend, Guarded, Hooks,
};
use haste_core::model::Source;
use haste_google_oauth::keychain::{MemoryStore, TokenStore};
use haste_google_oauth::testing::{spawn, FakeServer, Recorded};
use serde_json::{json, Value};

const NOW: i64 = 1_794_218_400; // 2026-11-09T10:00:00Z
const TOKEN: &str = "SECRET-BEEPER-TOKEN";
const NAME_MARKER: &str = "PRIVATE-CHAT-NAME";
const TEXT_MARKER: &str = "PRIVATE-MESSAGE-TEXT";

#[derive(Default)]
struct World {
    accounts: Vec<Value>,
    chats: Vec<Value>,
    /// chat id -> pages of messages (a page index is the cursor `pN`)
    pages: HashMap<String, Vec<Vec<Value>>>,
    send: Option<(u16, String)>,
    info_status: u16,
    /// When true the chat list answers empty (a selected chat beyond the first page).
    hide_chat_list: bool,
}

type Shared = Arc<Mutex<World>>;

fn serve(world: &Shared) -> impl Fn(&Recorded) -> (u16, String) + Send + Sync + 'static {
    let world = Arc::clone(world);
    move |r| {
        let w = world.lock().unwrap();
        let segs: Vec<&str> = r.path.trim_start_matches('/').split('/').collect();
        match (r.method.as_str(), segs.as_slice()) {
            ("GET", ["v1", "info"]) => (
                if w.info_status == 0 {
                    200
                } else {
                    w.info_status
                },
                r#"{"version":"x"}"#.into(),
            ),
            ("GET", ["v1", "accounts"]) => (
                if w.info_status == 0 {
                    200
                } else {
                    w.info_status
                },
                json!({ "items": w.accounts }).to_string(),
            ),
            ("GET", ["v1", "chats"]) => (
                200,
                json!({ "items": if w.hide_chat_list { vec![] } else { w.chats.clone() } })
                    .to_string(),
            ),
            ("GET", ["v1", "chats", id]) => match w.chats.iter().find(|c| c["id"] == *id) {
                Some(c) => (200, c.to_string()),
                None => (404, "{}".into()),
            },
            ("GET", ["v1", "chats", id, "messages"]) => {
                let pages = w.pages.get(*id).cloned().unwrap_or_default();
                let idx = r
                    .query
                    .split('&')
                    .find_map(|p| p.strip_prefix("cursor=p"))
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                let items = pages.get(idx).cloned().unwrap_or_default();
                (
                    200,
                    json!({ "items": items, "hasMore": idx + 1 < pages.len(), "nextCursor": format!("p{}", idx + 1) }).to_string(),
                )
            }
            ("POST", ["v1", "assets", "upload", "base64"]) => {
                (200, json!({ "uploadID": "up-1" }).to_string())
            }
            ("POST", ["v1", "chats", _, "messages", _, "reactions"]) => {
                (200, json!({ "success": true }).to_string())
            }
            ("POST", ["v1", "chats", _, "messages"]) => w
                .send
                .clone()
                .unwrap_or((200, json!({ "id": "msg-1" }).to_string())),
            _ => (404, "{}".into()),
        }
    }
}

fn account(id: &str, network: &str, connection: Option<&str>) -> Value {
    let mut a = json!({ "id": id, "network": network });
    if let Some(c) = connection {
        a["connectionType"] = json!(c);
    }
    a
}

fn chat(id: &str, account: &str, title: &str) -> Value {
    json!({ "id": id, "accountID": account, "title": title })
}

fn msg(id: &str, key: u32, ts: i64, text: &str) -> Value {
    let stamp = format_ts(ts);
    json!({ "id": id, "senderID": "user-1", "text": text, "timestamp": stamp, "sortKey": format!("{key:06}") })
}

fn format_ts(ts: i64) -> String {
    // civil date from unix seconds, UTC
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

struct Rig {
    server: FakeServer,
    world: Shared,
    connector: BeeperConnector,
    audits: Arc<Mutex<Vec<String>>>,
}

fn config(accounts: &[(&str, bool)], allow: &[&str], deny: &[&str]) -> BeeperConfig {
    BeeperConfig {
        accounts: accounts
            .iter()
            .map(|(id, att)| AccountSelection {
                id: (*id).into(),
                on_device_attested: *att,
                allow_cloud: false,
            })
            .collect(),
        chats: ChatSelection {
            allow_all: false,
            allow: allow.iter().map(|s| (*s).into()).collect(),
            deny: deny.iter().map(|s| (*s).into()).collect(),
        },
        history_days: 30,
        base_url: None,
    }
}

async fn rig(which: BeeperSource, setup: impl FnOnce(&mut World), cfg: BeeperConfig) -> Rig {
    let world: Shared = Arc::new(Mutex::new(World::default()));
    setup(&mut world.lock().unwrap());
    let server = spawn(serve(&world)).await;
    let store = Arc::new(MemoryStore::default());
    store.set(TOKEN_ACCOUNT, TOKEN).unwrap();
    let client = Arc::new(BeeperClient::new(&server.base, store).unwrap());
    let audits = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&audits);
    let audit: CycleAudit = Arc::new(move |o, d| sink.lock().unwrap().push(format!("{o}:{d}")));
    let connector = BeeperConnector::new(which, client, &cfg, audit).with_clock(Arc::new(|| NOW));
    Rig {
        server,
        world,
        connector,
        audits,
    }
}

fn basic(w: &mut World) {
    w.accounts = vec![
        account("acc-wa", "whatsapp", Some("on-device")),
        account("acc-ms", "facebook", Some("on-device")),
    ];
    w.chats = vec![
        chat("chat-a", "acc-wa", NAME_MARKER),
        chat("chat-b", "acc-wa", "Not selected"),
        chat("chat-c", "acc-wa", "Denied"),
        chat("chat-m", "acc-ms", "Messenger chat"),
    ];
    for id in ["chat-a", "chat-b", "chat-c", "chat-m"] {
        w.pages.insert(
            id.into(),
            vec![vec![
                msg("m2", 2, NOW - 60, TEXT_MARKER),
                msg("m1", 1, NOW - 120, "first"),
            ]],
        );
    }
}

fn gets(server: &FakeServer, suffix: &str) -> usize {
    server
        .requests()
        .iter()
        .filter(|r| r.method == "GET" && r.path.ends_with(suffix))
        .count()
}

fn posts(server: &FakeServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .count()
}

#[tokio::test]
async fn a_cycle_reads_only_selected_non_denied_chats_of_this_networks_selected_accounts() {
    let cfg = config(
        &[("acc-wa", false), ("acc-ms", false)],
        &["chat-a", "chat-c", "chat-m"],
        &["chat-c"],
    );
    let r = rig(BeeperSource::Whatsapp, basic, cfg).await;
    let batch = r.connector.fetch(None).await.unwrap();
    assert_eq!(batch.threads.len(), 1);
    let t = &batch.threads[0];
    assert_eq!(
        (t.external_id.as_str(), t.source.clone()),
        ("chat-a", Source::Whatsapp)
    );
    assert_eq!(t.subject, NAME_MARKER, "the chat name is stored locally");
    assert_eq!(batch.messages.len(), 2);
    assert!(batch
        .messages
        .iter()
        .all(|m| m.thread_id == "chat-a" && m.source == Source::Whatsapp));
    // unselected, denied and other-network chats are never requested
    for never in ["chat-b/messages", "chat-c/messages", "chat-m/messages"] {
        assert_eq!(gets(&r.server, never), 0, "{never}");
    }
    assert_eq!(gets(&r.server, "chat-a/messages"), 1);
    let cursor = BeeperCursor::decode(&batch.next_cursor.unwrap().0);
    assert_eq!(
        cursor.chats.get("chat-a").map(String::as_str),
        Some("000002")
    );
    // the audit has counts only
    let joined = r.audits.lock().unwrap().join("|");
    assert_eq!(
        joined,
        "ok:accounts=1 chats=1 messages=2 skipped_chats=0 skipped_messages=0"
    );
    for secret in [NAME_MARKER, TEXT_MARKER, TOKEN, "acc-wa", "chat-a"] {
        assert!(!joined.contains(secret), "audit leaked {secret}");
    }
}

#[tokio::test]
async fn the_messenger_registration_reads_only_its_own_network() {
    let cfg = config(
        &[("acc-wa", false), ("acc-ms", false)],
        &["chat-a", "chat-m"],
        &[],
    );
    let r = rig(BeeperSource::Messenger, basic, cfg).await;
    let batch = r.connector.fetch(None).await.unwrap();
    assert_eq!(batch.threads.len(), 1);
    assert_eq!(batch.threads[0].source, Source::Messenger);
    assert_eq!(gets(&r.server, "chat-a/messages"), 0);
}

fn bridge_account(id: &str, provider: &str) -> Value {
    json!({ "accountID": id, "network": "WhatsApp", "status": "connected",
            "bridge": { "id": "whatsapp", "type": "whatsapp", "provider": provider } })
}

fn one_chat_world(provider: &'static str) -> impl FnOnce(&mut World) {
    move |w: &mut World| {
        w.accounts = vec![bridge_account("acc-wa", provider)];
        w.chats = vec![chat("chat-a", "acc-wa", NAME_MARKER)];
        w.pages
            .insert("chat-a".into(), vec![vec![msg("m1", 1, NOW - 60, "hi")]]);
    }
}

#[tokio::test]
async fn bridge_provider_decides_on_device_and_cloud_is_refused_even_when_attested() {
    // local: accepted without any attestation
    let r = rig(
        BeeperSource::Whatsapp,
        one_chat_world("local"),
        config(&[("acc-wa", false)], &["chat-a"], &[]),
    )
    .await;
    assert_eq!(r.connector.fetch(None).await.unwrap().threads.len(), 1);
    // cloud: refused whatever the attestation says, and no chat is requested
    let r = rig(
        BeeperSource::Whatsapp,
        one_chat_world("cloud"),
        config(&[("acc-wa", true)], &["chat-a"], &[]),
    )
    .await;
    assert!(r.connector.fetch(None).await.is_err());
    assert_eq!(gets(&r.server, "chat-a"), 0);
    // self-hosted does not say "this device": needs the human's attestation
    let r = rig(
        BeeperSource::Whatsapp,
        one_chat_world("self-hosted"),
        config(&[("acc-wa", false)], &["chat-a"], &[]),
    )
    .await;
    assert!(r.connector.fetch(None).await.is_err());
    let r = rig(
        BeeperSource::Whatsapp,
        one_chat_world("self-hosted"),
        config(&[("acc-wa", true)], &["chat-a"], &[]),
    )
    .await;
    assert_eq!(r.connector.fetch(None).await.unwrap().threads.len(), 1);
}

#[tokio::test]
async fn a_cloud_account_is_used_only_when_that_account_allows_cloud() {
    // allowed for acc-wa only: that account reads, and the audit names how it was accepted
    let mut cfg = config(&[("acc-wa", false)], &["chat-a"], &[]);
    cfg.accounts[0].allow_cloud = true;
    let r = rig(BeeperSource::Whatsapp, one_chat_world("cloud"), cfg).await;
    assert_eq!(r.connector.fetch(None).await.unwrap().threads.len(), 1);
    // a different account is not covered by it
    let mut cfg = config(&[("acc-wa", false), ("acc-x", false)], &["chat-a"], &[]);
    cfg.accounts[1].allow_cloud = true;
    let r = rig(BeeperSource::Whatsapp, one_chat_world("cloud"), cfg).await;
    let err = r.connector.fetch(None).await.unwrap_err();
    assert!(format!("{err:?}").contains("allow_cloud"), "{err:?}");
    assert_eq!(gets(&r.server, "chat-a"), 0);
}

#[tokio::test]
async fn a_selected_chat_is_fetched_by_id_so_a_quiet_one_is_never_missed_and_a_stale_id_is_skipped()
{
    let r = rig(
        BeeperSource::Whatsapp,
        |w: &mut World| {
            one_chat_world("local")(w);
            // the chat list (newest first, paginated) is empty: only a by-id fetch finds the chat
            w.hide_chat_list = true;
        },
        config(&[("acc-wa", false)], &["chat-a", "gone"], &[]),
    )
    .await;
    let batch = r.connector.fetch(None).await.unwrap();
    assert_eq!(batch.threads.len(), 1);
    assert_eq!(
        gets(&r.server, "/v1/chats"),
        0,
        "the chat list is never requested"
    );
    assert!(r.audits.lock().unwrap()[0].contains("skipped_chats=1"));
}

#[tokio::test]
async fn nothing_is_read_without_selection() {
    let r = rig(BeeperSource::Whatsapp, basic, BeeperConfig::default()).await;
    let batch = r.connector.fetch(None).await.unwrap();
    assert!(batch.threads.is_empty() && batch.messages.is_empty());
    assert!(
        r.server
            .requests()
            .iter()
            .all(|q| !q.path.contains("/chats")),
        "not even the chat list"
    );
    // an account selected but no chat allowed: the list is read, no messages
    let cfg = config(&[("acc-wa", false)], &[], &[]);
    let r = rig(BeeperSource::Whatsapp, basic, cfg).await;
    r.connector.fetch(None).await.unwrap();
    assert_eq!(gets(&r.server, "/messages"), 0);
}

#[tokio::test]
async fn the_first_read_obeys_the_history_window_and_the_backfill_cap() {
    let cfg = config(&[("acc-wa", false)], &["chat-a"], &[]);
    let r = rig(
        BeeperSource::Whatsapp,
        |w| {
            basic(w);
            let day = 86_400;
            let mut recent: Vec<Value> = (0..(BEEPER_BACKFILL_PER_CHAT + 40))
                .map(|i| {
                    msg(
                        &format!("r{i}"),
                        1000 + i as u32,
                        NOW - 100 - i as i64,
                        "recent",
                    )
                })
                .collect();
            recent.push(msg("old", 1, NOW - 40 * day, "outside the 30 day window"));
            // one page so the cap, not the page limit, decides
            w.pages.insert("chat-a".into(), vec![recent]);
        },
        cfg,
    )
    .await;
    let batch = r.connector.fetch(None).await.unwrap();
    assert_eq!(batch.messages.len(), BEEPER_BACKFILL_PER_CHAT);
    assert!(
        batch.messages.iter().all(|m| m.body_text == "recent"),
        "the old message is outside the window"
    );
}

#[tokio::test]
async fn pages_are_bounded_and_a_delta_reads_only_what_is_newer_than_the_cursor() {
    let cfg = config(&[("acc-wa", false)], &["chat-a"], &[]);
    let r = rig(
        BeeperSource::Whatsapp,
        |w| {
            basic(w);
            // more pages than the limit allows, each with fresh messages
            let pages: Vec<Vec<Value>> = (0..BEEPER_MAX_PAGES_PER_CHAT + 3)
                .map(|p| {
                    vec![msg(
                        &format!("p{p}"),
                        100 - p as u32,
                        NOW - 10 - p as i64,
                        "x",
                    )]
                })
                .collect();
            w.pages.insert("chat-a".into(), pages);
        },
        cfg,
    )
    .await;
    let first = r.connector.fetch(None).await.unwrap();
    assert_eq!(
        first.messages.len(),
        BEEPER_MAX_PAGES_PER_CHAT,
        "one message per page, page-limited"
    );
    assert_eq!(
        gets(&r.server, "chat-a/messages"),
        BEEPER_MAX_PAGES_PER_CHAT
    );

    // second cycle: nothing newer than the cursor, so nothing new is returned (idempotent re-read)
    r.world
        .lock()
        .unwrap()
        .pages
        .insert("chat-a".into(), vec![vec![msg("p0", 100, NOW - 10, "x")]]);
    let second = r.connector.fetch(first.next_cursor.clone()).await.unwrap();
    assert!(second.messages.is_empty() && second.threads.is_empty());
    assert_eq!(
        BeeperCursor::decode(&second.next_cursor.unwrap().0),
        BeeperCursor::decode(&first.next_cursor.unwrap().0)
    );

    // a newer message arrives: only it is returned
    r.world.lock().unwrap().pages.insert(
        "chat-a".into(),
        vec![vec![
            msg("new", 200, NOW + 5, "fresh"),
            msg("p0", 100, NOW - 10, "x"),
        ]],
    );
    let third = r
        .connector
        .fetch(Some(Cursor(
            BeeperCursor {
                v: haste_connector_beeper::connector::CURSOR_VERSION,
                chats: [("chat-a".to_owned(), "000100".to_owned())].into(),
            }
            .encode(),
        )))
        .await
        .unwrap();
    assert_eq!(third.messages.len(), 1);
    assert_eq!(third.messages[0].external_id, "new");

    // a cursor from before the current layout (no version) reads the history again, so fields
    // added since (sender names) are filled in for chats stored earlier
    let old = r
        .connector
        .fetch(Some(Cursor(r#"{"chats":{"chat-a":"000300"}}"#.to_owned())))
        .await
        .unwrap();
    assert_eq!(old.messages.len(), 2, "both messages are read again");
    assert_eq!(
        BeeperCursor::decode(&old.next_cursor.unwrap().0).v,
        haste_connector_beeper::connector::CURSOR_VERSION
    );
}

#[tokio::test]
async fn long_bodies_are_cut_and_non_text_messages_become_a_label() {
    let cfg = config(&[("acc-wa", false)], &["chat-a"], &[]);
    let r = rig(
        BeeperSource::Whatsapp,
        |w| {
            basic(w);
            let long = "é".repeat(BEEPER_BODY_MAX_BYTES);
            let mut media = msg("media", 3, NOW - 30, "holiday-photo.jpg");
            media["type"] = json!("image");
            w.pages.insert(
                "chat-a".into(),
                vec![vec![msg("long", 4, NOW - 20, &long), media]],
            );
        },
        cfg,
    )
    .await;
    let batch = r.connector.fetch(None).await.unwrap();
    let by_id: HashMap<_, _> = batch
        .messages
        .iter()
        .map(|m| (m.external_id.as_str(), m))
        .collect();
    assert!(by_id["long"].body_text.len() <= BEEPER_BODY_MAX_BYTES + 20);
    assert!(by_id["long"].body_text.ends_with("[truncated]"));
    assert_eq!(
        by_id["media"].body_text, ATTACHMENT_LABEL,
        "an attachment's name is never stored"
    );
}

#[tokio::test]
async fn cloud_connections_unknown_types_and_unsupported_networks_are_refused_with_a_reason() {
    let selected = config(&[("acc-wa", false)], &["chat-a"], &[]);
    // reported as cloud: refused even if attested
    let attested = config(&[("acc-wa", true)], &["chat-a"], &[]);
    let r = rig(
        BeeperSource::Whatsapp,
        |w| {
            basic(w);
            w.accounts = vec![account("acc-wa", "whatsapp", Some("cloud"))];
        },
        attested.clone(),
    )
    .await;
    let err = r.connector.fetch(None).await.unwrap_err();
    assert!(
        matches!(&err, ConnectorError::Other(m) if m.starts_with("refused: cloud connection")),
        "{err:?}"
    );
    assert_eq!(gets(&r.server, "/messages"), 0);
    assert_eq!(
        r.audits.lock().unwrap().as_slice(),
        ["refused_or_error:cloud_connection"]
    );
    // the API does not say: refused without attestation, allowed with it
    let r = rig(
        BeeperSource::Whatsapp,
        |w| {
            basic(w);
            w.accounts = vec![account("acc-wa", "whatsapp", None)];
        },
        selected,
    )
    .await;
    assert!(
        matches!(r.connector.fetch(None).await, Err(ConnectorError::Other(m)) if m.contains("unknown"))
    );
    let r = rig(
        BeeperSource::Whatsapp,
        |w| {
            basic(w);
            w.accounts = vec![account("acc-wa", "whatsapp", None)];
        },
        attested,
    )
    .await;
    assert_eq!(r.connector.fetch(None).await.unwrap().threads.len(), 1);
    // an account on a network we do not map
    let r = rig(
        BeeperSource::Whatsapp,
        |w| {
            basic(w);
            w.accounts = vec![account("acc-wa", "signal", Some("on-device"))];
        },
        config(&[("acc-wa", false)], &["chat-a"], &[]),
    )
    .await;
    assert!(
        matches!(r.connector.fetch(None).await, Err(ConnectorError::Other(m)) if m.contains("unsupported network"))
    );
}

#[tokio::test]
async fn beeper_not_running_a_missing_or_a_rejected_token_wait_for_the_human_and_never_trip_the_breaker(
) {
    // not running: a port nothing listens on
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let store = Arc::new(MemoryStore::default());
    store.set(TOKEN_ACCOUNT, TOKEN).unwrap();
    let client = Arc::new(BeeperClient::new(&format!("http://127.0.0.1:{port}"), store).unwrap());
    let audit: CycleAudit = Arc::new(|_, _| {});
    let c = BeeperConnector::new(
        BeeperSource::Whatsapp,
        client,
        &config(&[("a", true)], &["c"], &[]),
        audit,
    );
    let err = c.fetch(None).await.unwrap_err();
    assert!(
        matches!(&err, ConnectorError::Authorization(m) if m.contains("not running")),
        "{err:?}"
    );
    assert!(!format!("{err:?}").contains(TOKEN));

    // no token stored
    let server = spawn(|_| (200, "{}".into())).await;
    let client =
        Arc::new(BeeperClient::new(&server.base, Arc::new(MemoryStore::default())).unwrap());
    let c = BeeperConnector::new(
        BeeperSource::Whatsapp,
        client,
        &config(&[("a", true)], &["c"], &[]),
        Arc::new(|_, _| {}),
    );
    assert!(
        matches!(c.fetch(None).await, Err(ConnectorError::Authorization(m)) if m.contains("no Beeper token"))
    );
    assert!(server.requests().is_empty(), "no request without a token");

    // token rejected
    let server = spawn(|_| (401, "{}".into())).await;
    let store = Arc::new(MemoryStore::default());
    store.set(TOKEN_ACCOUNT, TOKEN).unwrap();
    let client = Arc::new(BeeperClient::new(&server.base, store).unwrap());
    let c = BeeperConnector::new(
        BeeperSource::Whatsapp,
        client,
        &config(&[("a", true)], &["c"], &[]),
        Arc::new(|_, _| {}),
    );
    let err = c.fetch(None).await.unwrap_err();
    assert!(matches!(&err, ConnectorError::Authorization(m) if m.contains("token rejected")));
    assert_eq!(server.requests().len(), 1, "no retry loop");

    // through the guard: no breaker, a permission-needed audit
    #[derive(Default)]
    struct Hk(Mutex<Vec<String>>);
    impl Hooks for Hk {
        fn kill_switch_engaged(&self, _: &Source) -> bool {
            false
        }
        fn set_kill_switch(&self, _: &Source, e: bool) {
            self.0.lock().unwrap().push(format!("engaged={e}"));
        }
        fn audit(&self, a: &str, _: &Source, o: &str) {
            self.0.lock().unwrap().push(format!("{a}:{o}"));
        }
        fn notify(&self, _: &str) {}
    }
    let hooks = Hk::default();
    let guard = Guarded {
        connector: &c,
        hooks: &hooks,
    };
    assert!(guard.fetch(None).await.is_err());
    let events = hooks.0.lock().unwrap().clone();
    assert!(
        events.contains(&"fetch:permission_needed".to_owned()),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("engaged=")),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_redirect_is_never_followed_so_the_token_cannot_be_moved_elsewhere() {
    let elsewhere = spawn(|_| (200, r#"{"items":[]}"#.into())).await;
    let target = format!("{}/stolen", elsewhere.base);
    let server = spawn(move |_| (302, format!("{{\"to\":\"{target}\"}}"))).await;
    let store = Arc::new(MemoryStore::default());
    store.set(TOKEN_ACCOUNT, TOKEN).unwrap();
    let client = Arc::new(BeeperClient::new(&server.base, store).unwrap());
    let c = BeeperConnector::new(
        BeeperSource::Whatsapp,
        client,
        &config(&[("a", true)], &["c"], &[]),
        Arc::new(|_, _| {}),
    );
    assert!(c.fetch(None).await.is_err());
    assert!(
        elsewhere.requests().is_empty(),
        "the redirect target saw nothing"
    );
}

// ------------------------------------------------------------------ human send

fn reply(chat: &str, text: &str) -> DraftToSend {
    DraftToSend {
        attachments: Vec::new(),
        thread_id: Some(chat.into()),
        to: None,
        body: text.into(),
        recipients: vec![],
        subject: None,
        approval_draft_id: None,
    }
}

fn send_rig_cfg() -> BeeperConfig {
    config(&[("acc-wa", false)], &["chat-a"], &["chat-c"])
}

#[tokio::test]
async fn a_send_makes_one_text_only_request_into_a_selected_on_device_chat() {
    let r = rig(BeeperSource::Whatsapp, basic, send_rig_cfg()).await;
    let receipt = r
        .connector
        .send(&reply("chat-a", "On my way"))
        .await
        .unwrap();
    assert_eq!(receipt.external_id, "beeper:msg-1");
    assert_eq!(posts(&r.server), 1);
    let post = r
        .server
        .requests()
        .into_iter()
        .find(|q| q.method == "POST")
        .unwrap();
    assert_eq!(post.path, "/v1/chats/chat-a/messages");
    let body: Value = serde_json::from_str(&post.body).unwrap();
    assert_eq!(
        body,
        json!({ "text": "On my way" }),
        "text only: no attachment, no extra field"
    );
}

#[tokio::test]
async fn a_file_is_uploaded_to_the_local_service_then_sent_with_the_text() {
    use haste_core::connector::OutAttachment;
    let r = rig(BeeperSource::Whatsapp, basic, send_rig_cfg()).await;
    let file = |name: &str| OutAttachment {
        name: name.into(),
        mime: "application/pdf".into(),
        data: b"%PDF-1.4 test".to_vec(),
    };
    let draft = DraftToSend {
        attachments: vec![file("report.pdf")],
        ..reply("chat-a", "Here it is")
    };
    let receipt = r.connector.send(&draft).await.unwrap();
    assert_eq!(receipt.external_id, "beeper:msg-1");
    let reqs: Vec<_> = r
        .server
        .requests()
        .into_iter()
        .filter(|q| q.method == "POST")
        .collect();
    assert_eq!(reqs.len(), 2, "one upload, one message");
    assert_eq!(reqs[0].path, "/v1/assets/upload/base64");
    let up: Value = serde_json::from_str(&reqs[0].body).unwrap();
    assert_eq!(up["fileName"], "report.pdf");
    assert_eq!(reqs[1].path, "/v1/chats/chat-a/messages");
    let body: Value = serde_json::from_str(&reqs[1].body).unwrap();
    assert_eq!(body["text"], "Here it is");
    assert_eq!(body["attachment"]["uploadID"], "up-1");

    // a file alone is fine in a chat; two files in one message are refused before any request
    let only_file = DraftToSend {
        attachments: vec![file("a.pdf")],
        ..reply("chat-a", "")
    };
    assert!(r.connector.send(&only_file).await.is_ok());
    let before = r.server.requests().len();
    let two = DraftToSend {
        attachments: vec![file("a.pdf"), file("b.pdf")],
        ..reply("chat-a", "x")
    };
    assert!(r.connector.send(&two).await.is_err());
    assert_eq!(r.server.requests().len(), before, "nothing was requested");
}

#[tokio::test]
async fn every_refusal_before_the_request_sends_nothing() {
    let r = rig(BeeperSource::Whatsapp, basic, send_rig_cfg()).await;
    for (draft, why) in [
        (reply("chat-b", "x"), "chat not selected"),
        (reply("chat-c", "x"), "chat not selected"),
        (reply("chat-a", "   "), "body_length"),
        (
            reply("chat-a", &"y".repeat(BEEPER_MAX_SEND_BYTES + 1)),
            "body_length",
        ),
        (
            DraftToSend {
                thread_id: None,
                ..reply("chat-a", "x")
            },
            "no chat",
        ),
    ] {
        let err = r.connector.send(&draft).await.unwrap_err();
        assert_eq!(
            err,
            ConnectorError::Other(format!("send refused: {why}")),
            "{why}"
        );
    }
    assert_eq!(posts(&r.server), 0);
    // a cloud account: refused before the message request
    let r = rig(
        BeeperSource::Whatsapp,
        |w| {
            basic(w);
            w.accounts = vec![account("acc-wa", "whatsapp", Some("cloud"))];
        },
        send_rig_cfg(),
    )
    .await;
    assert!(r.connector.send(&reply("chat-a", "x")).await.is_err());
    assert_eq!(posts(&r.server), 0);
    // a chat of another network: this registration refuses it
    let r = rig(
        BeeperSource::Imessage,
        basic,
        config(&[("acc-wa", false)], &["chat-a"], &[]),
    )
    .await;
    assert!(r.connector.send(&reply("chat-a", "x")).await.is_err());
    assert_eq!(posts(&r.server), 0);
}

#[tokio::test]
async fn each_failure_is_classified_once_with_no_retry() {
    for (status, body, expect) in [
        (401, "{}", "Authorization"),
        (403, "{}", "Authorization"),
        (429, "{}", "Checkpoint"),
        (404, "{}", "Other"),
        (400, "{}", "Other"),
        (500, "{}", "Ambiguous"),
        (503, "{}", "Ambiguous"),
        (200, "{}", "Ambiguous"),
        (200, r#"{"id":""}"#, "Ambiguous"),
        (200, "not json", "Ambiguous"),
    ] {
        let r = rig(
            BeeperSource::Whatsapp,
            |w| {
                basic(w);
                w.send = Some((status, body.to_owned()));
            },
            send_rig_cfg(),
        )
        .await;
        let err = r.connector.send(&reply("chat-a", "x")).await.unwrap_err();
        let got = match &err {
            ConnectorError::Authorization(_) => "Authorization",
            ConnectorError::Checkpoint(_) => "Checkpoint",
            ConnectorError::Other(m) => {
                assert!(m.starts_with("send refused"), "{m}");
                "Other"
            }
            ConnectorError::Ambiguous(_) => "Ambiguous",
            other => panic!("{status}: {other:?}"),
        };
        assert_eq!(got, expect, "{status} {body}");
        assert_eq!(
            posts(&r.server),
            1,
            "{status}: exactly one request, never retried"
        );
        assert!(!format!("{err:?}").contains(TOKEN));
    }
}

#[tokio::test]
async fn a_missing_messages_window_means_nothing_was_sent() {
    // a network other than iMessage, so the test never opens the real Messages app
    let r = rig(
        BeeperSource::Whatsapp,
        |w| {
            basic(w);
            w.send = Some((
                500,
                r#"{"message":"Timed out verifying parked Messages AX main window"}"#.to_owned(),
            ));
        },
        send_rig_cfg(),
    )
    .await;
    let err = r.connector.send(&reply("chat-a", "x")).await.unwrap_err();
    match &err {
        ConnectorError::Other(m) => assert!(m.contains("nothing was sent"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(posts(&r.server), 1, "one request, never retried");
}

#[test]
fn the_connector_starts_engaged_declares_no_drafts_and_owns_one_network() {
    let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
    let client = Arc::new(BeeperClient::new("http://127.0.0.1:23373", store).unwrap());
    for (which, source) in [
        (BeeperSource::Whatsapp, Source::Whatsapp),
        (BeeperSource::Messenger, Source::Messenger),
        (BeeperSource::Imessage, Source::Imessage),
    ] {
        let c = BeeperConnector::new(
            which,
            Arc::clone(&client),
            &BeeperConfig::default(),
            Arc::new(|_, _| {}),
        );
        assert_eq!(c.source(), source);
        assert!(c.default_engaged());
        let caps = c.capabilities();
        assert!(caps.read && caps.send && !caps.draft && !caps.archive);
        let _ = CallError::KillSwitchEngaged;
    }
}

#[tokio::test]
async fn allow_all_reads_every_chat_of_the_selected_accounts_except_denied_ones() {
    let mut cfg = config(&[("acc-wa", false), ("acc-ms", false)], &[], &["chat-c"]);
    cfg.chats.allow_all = true;
    let r = rig(BeeperSource::Whatsapp, basic, cfg).await;
    let batch = r.connector.fetch(None).await.unwrap();
    let mut ids: Vec<&str> = batch
        .threads
        .iter()
        .map(|t| t.external_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["chat-a", "chat-b"],
        "this network only, denied chat skipped"
    );
    for never in ["chat-c/messages", "chat-m/messages"] {
        assert_eq!(gets(&r.server, never), 0, "{never}");
    }
    let joined = r.audits.lock().unwrap().join("|");
    assert_eq!(
        joined,
        "ok:accounts=1 chats=2 messages=4 skipped_chats=0 skipped_messages=0"
    );
}

#[tokio::test]
async fn a_reaction_is_one_request_into_a_selected_chat_and_refusals_send_nothing() {
    use haste_core::connector::ReactionRequest;
    let r = rig(BeeperSource::Whatsapp, basic, send_rig_cfg()).await;
    let ask = |chat: &str, key: &str| ReactionRequest {
        thread_external_id: chat.into(),
        message_external_id: "msg-9".into(),
        key: key.into(),
    };
    r.connector
        .add_reaction(&ask("chat-a", "👍"))
        .await
        .unwrap();
    let post = r
        .server
        .requests()
        .into_iter()
        .find(|q| q.method == "POST")
        .unwrap();
    assert_eq!(post.path, "/v1/chats/chat-a/messages/msg-9/reactions");
    let body: Value = serde_json::from_str(&post.body).unwrap();
    assert_eq!(body, json!({ "reactionKey": "👍" }));
    assert_eq!(posts(&r.server), 1);
    // a chat that is not selected, or an unusable key, never reaches the service
    assert!(r
        .connector
        .add_reaction(&ask("chat-b", "👍"))
        .await
        .is_err());
    assert!(r
        .connector
        .add_reaction(&ask("chat-a", "a]b"))
        .await
        .is_err());
    assert_eq!(posts(&r.server), 1, "nothing more was requested");
}
