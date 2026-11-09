//! Connector behaviour against a local fake of Google's endpoints. No live calls.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use haste_connector_gmail::auth::{
    pkce_challenge, run_consent, AuthError, OAuthClient, OAuthEndpoints, TokenSource,
};
use haste_connector_gmail::keychain::{MemoryStore, TokenStore};
use haste_connector_gmail::testing::{spawn, FakeServer, Recorded};
use haste_connector_gmail::{GmailConfig, GmailConnector};
use haste_core::connector::{
    CallError, Connector, ConnectorError, Cursor, DraftToSend, Guarded, Hooks,
    TRANSIENT_ATTEMPTS_PER_CYCLE,
};
use haste_core::model::Source;

const THREAD_FULL: &str = include_str!("fixtures/thread_three_messages.json");

fn config() -> GmailConfig {
    GmailConfig {
        client_id: "cid".into(),
        client_secret: "secret".into(),
        account: "me@example.com".into(),
    }
}

fn store_with_refresh() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store.set("me@example.com", "REFRESH-TOKEN").unwrap();
    store
}

fn connector(server: &FakeServer, store: Arc<MemoryStore>) -> GmailConnector {
    GmailConnector::new(
        &config(),
        &server.base,
        &format!("{}/token", server.base),
        store,
    )
}

/// Routes shared by most tests; `history` decides what the history endpoint says.
fn routes(
    history: impl Fn() -> (u16, String) + Send + Sync + 'static,
) -> impl Fn(&Recorded) -> (u16, String) + Send + Sync + 'static {
    move |r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/token") => (200, r#"{"access_token":"AT","expires_in":3600}"#.into()),
        ("GET", "/gmail/v1/users/me/profile") => (
            200,
            r#"{"emailAddress":"me@example.com","historyId":"900"}"#.into(),
        ),
        ("GET", "/gmail/v1/users/me/threads") => (200, r#"{"threads":[{"id":"t100"}]}"#.into()),
        ("GET", "/gmail/v1/users/me/threads/t100") => (200, THREAD_FULL.into()),
        ("GET", "/gmail/v1/users/me/history") => history(),
        ("POST", "/gmail/v1/users/me/drafts") => {
            (200, r#"{"id":"draft-1","message":{"id":"x"}}"#.into())
        }
        _ => (404, "{}".into()),
    }
}

#[derive(Default)]
struct TestHooks {
    engaged: Mutex<HashSet<String>>,
    audits: Mutex<Vec<String>>,
    notices: Mutex<Vec<String>>,
}

impl Hooks for TestHooks {
    fn kill_switch_engaged(&self, s: &Source) -> bool {
        self.engaged.lock().unwrap().contains(&s.as_name())
    }
    fn set_kill_switch(&self, s: &Source, on: bool) {
        let mut set = self.engaged.lock().unwrap();
        if on {
            set.insert(s.as_name());
        } else {
            set.remove(&s.as_name());
        }
    }
    fn audit(&self, action: &str, _s: &Source, outcome: &str) {
        self.audits
            .lock()
            .unwrap()
            .push(format!("{action}:{outcome}"));
    }
    fn notify(&self, m: &str) {
        self.notices.lock().unwrap().push(m.to_owned());
    }
}

#[test]
fn capabilities_are_read_and_draft_only() {
    let c = GmailConnector::from_config(&config(), Arc::new(MemoryStore::default()));
    let caps = c.capabilities();
    assert!(caps.read && caps.draft && !caps.send && !caps.archive);
    assert_eq!(c.source(), Source::Gmail);
}

#[tokio::test]
async fn first_cycle_backfills_and_sets_the_history_cursor() {
    let server = spawn(routes(|| (200, "{}".into()))).await;
    let c = connector(&server, store_with_refresh());
    let batch = c.fetch(None).await.unwrap();
    assert_eq!(batch.threads.len(), 1);
    assert_eq!(batch.messages.len(), 3);
    assert_eq!(batch.next_cursor, Some(Cursor("900".into())));
    let list = server
        .requests()
        .into_iter()
        .find(|r| r.path.ends_with("/threads"))
        .unwrap();
    assert!(
        list.query.contains("newer_than%3A30d") || list.query.contains("newer_than:30d"),
        "{}",
        list.query
    );
    assert_eq!(server.count("/token"), 1, "one refresh serves every call");
}

#[tokio::test]
async fn steady_state_delta_fetches_only_changed_threads_in_full() {
    let server = spawn(routes(|| {
        (200, r#"{"history":[{"messages":[{"id":"m3","threadId":"t100"}]},{"messages":[{"id":"m2","threadId":"t100"}]}],"historyId":"950"}"#.into())
    }))
    .await;
    let c = connector(&server, store_with_refresh());
    let batch = c.fetch(Some(Cursor("900".into()))).await.unwrap();
    assert_eq!(
        batch.threads.len(),
        1,
        "duplicate history entries collapse to one thread"
    );
    assert_eq!(batch.messages.len(), 3, "the thread is re-fetched in full");
    assert_eq!(batch.next_cursor, Some(Cursor("950".into())));
    assert_eq!(server.count("/gmail/v1/users/me/threads/t100"), 1);
    assert_eq!(server.count("/gmail/v1/users/me/profile"), 0, "no backfill");
}

#[tokio::test]
async fn empty_delta_returns_an_empty_batch_with_the_same_cursor() {
    let server = spawn(routes(|| (200, r#"{"historyId":"999"}"#.into()))).await;
    let c = connector(&server, store_with_refresh());
    let batch = c.fetch(Some(Cursor("900".into()))).await.unwrap();
    assert!(batch.threads.is_empty() && batch.messages.is_empty());
    assert_eq!(batch.next_cursor, Some(Cursor("900".into())));
}

#[tokio::test]
async fn expired_history_falls_back_to_one_bounded_backfill_without_looping() {
    let server = spawn(routes(|| (404, r#"{"error":{"code":404}}"#.into()))).await;
    let c = connector(&server, store_with_refresh());
    let batch = c.fetch(Some(Cursor("1".into()))).await.unwrap();
    assert_eq!(batch.threads.len(), 1);
    assert_eq!(
        batch.next_cursor,
        Some(Cursor("900".into())),
        "cursor reset from the profile"
    );
    assert_eq!(
        server.count("/gmail/v1/users/me/history"),
        1,
        "history is asked exactly once"
    );
}

#[tokio::test]
async fn send_is_unsupported_and_the_guard_refuses_it_before_the_connector() {
    let server = spawn(routes(|| (200, "{}".into()))).await;
    let c = connector(&server, store_with_refresh());
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &c,
        hooks: &hooks,
    };
    let draft = DraftToSend {
        thread_id: None,
        to: Some("a@x.org".into()),
        body: "hi".into(),
    };
    assert_eq!(
        guard.send(&draft).await,
        Err(CallError::CapabilityMissing("send"))
    );
    assert!(hooks
        .audits
        .lock()
        .unwrap()
        .contains(&"send:refused_capability".to_owned()));
    assert!(server.requests().is_empty(), "nothing reached Google");
    // Even called directly, the connector itself refuses.
    assert!(matches!(
        c.send(&draft).await,
        Err(ConnectorError::Other(_))
    ));
}

#[tokio::test]
async fn draft_lands_in_the_thread_as_single_part_plain_text() {
    let server = spawn(routes(|| (200, "{}".into()))).await;
    let c = connector(&server, store_with_refresh());
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &c,
        hooks: &hooks,
    };
    let draft = DraftToSend {
        thread_id: Some("t100".into()),
        to: None,
        body: "Noon is great.\nSee you!".into(),
    };
    let receipt = guard.create_draft(&draft).await.unwrap();
    assert_eq!(receipt.draft_id, "draft-1");
    assert!(hooks
        .audits
        .lock()
        .unwrap()
        .contains(&"draft.create:ok".to_owned()));

    let post = server
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path.ends_with("/drafts"))
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&post.body).unwrap();
    assert_eq!(json["message"]["threadId"], "t100");
    let raw = json["message"]["raw"].as_str().unwrap();
    use base64::Engine;
    let mime = String::from_utf8(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .unwrap(),
    )
    .unwrap();
    assert!(mime.contains("Content-Type: text/plain; charset=UTF-8"));
    assert!(mime.contains("Subject: Re: Lunch on Friday?"), "{mime}");
    assert!(
        mime.contains("To: bela@example.org"),
        "replies to the last sender: {mime}"
    );
    assert!(
        !mime.to_lowercase().contains("multipart") && !mime.to_lowercase().contains("text/html")
    );
}

#[tokio::test]
async fn checkpoint_from_google_trips_the_breaker_with_no_retry() {
    let server = spawn(|r: &Recorded| match r.path.as_str() {
        "/token" => (200, r#"{"access_token":"AT","expires_in":3600}"#.into()),
        _ => (403, r#"{"error":{"message":"Account has been disabled.","errors":[{"reason":"accountDisabled"}]}}"#.into()),
    })
    .await;
    let c = connector(&server, store_with_refresh());
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &c,
        hooks: &hooks,
    };
    assert!(matches!(
        guard.fetch(None).await,
        Err(CallError::BreakerOpened(_))
    ));
    assert_eq!(server.count("/gmail/v1/users/me/profile"), 1, "no retry");
    assert!(hooks.engaged.lock().unwrap().contains("gmail"));
    assert_eq!(hooks.notices.lock().unwrap().len(), 1);
    assert_eq!(guard.fetch(None).await, Err(CallError::KillSwitchEngaged));
}

#[tokio::test]
async fn rate_limits_stop_after_the_per_cycle_budget() {
    let server = spawn(|r: &Recorded| match r.path.as_str() {
        "/token" => (200, r#"{"access_token":"AT","expires_in":3600}"#.into()),
        _ => (429, "{}".into()),
    })
    .await;
    let c = connector(&server, store_with_refresh());
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &c,
        hooks: &hooks,
    };
    assert!(matches!(
        guard.fetch(None).await,
        Err(CallError::TransientExhausted(_))
    ));
    assert_eq!(
        server.count("/gmail/v1/users/me/profile"),
        TRANSIENT_ATTEMPTS_PER_CYCLE as usize
    );
    assert!(
        !hooks.engaged.lock().unwrap().contains("gmail"),
        "rate limits do not trip the breaker"
    );
}

#[tokio::test]
async fn invalid_grant_engages_the_kill_switch_and_is_not_retried() {
    let server = spawn(|r: &Recorded| match r.path.as_str() {
        "/token" => (
            400,
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
                .into(),
        ),
        _ => (200, "{}".into()),
    })
    .await;
    let c = connector(&server, store_with_refresh());
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &c,
        hooks: &hooks,
    };
    assert!(matches!(
        guard.fetch(None).await,
        Err(CallError::BreakerOpened(ConnectorError::Checkpoint(_)))
    ));
    assert_eq!(server.count("/token"), 1);
    assert_eq!(server.count("/gmail"), 0);
    assert!(hooks.engaged.lock().unwrap().contains("gmail"));
    assert!(hooks.notices.lock().unwrap()[0].contains("circuit breaker"));
}

#[tokio::test]
async fn missing_refresh_token_fails_closed() {
    let server = spawn(routes(|| (200, "{}".into()))).await;
    let c = connector(&server, Arc::new(MemoryStore::default()));
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &c,
        hooks: &hooks,
    };
    assert!(matches!(
        guard.fetch(None).await,
        Err(CallError::BreakerOpened(_))
    ));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn access_token_is_cached_and_tokens_never_appear_in_errors() {
    let server = spawn(|r: &Recorded| match r.path.as_str() {
        "/token" => (
            200,
            r#"{"access_token":"AT-SECRET","expires_in":3600}"#.into(),
        ),
        _ => (500, "{}".into()),
    })
    .await;
    let http = reqwest::Client::new();
    let source = TokenSource::new(
        http,
        &format!("{}/token", server.base),
        OAuthClient {
            client_id: "cid".into(),
            client_secret: "s".into(),
        },
        "me@example.com",
        store_with_refresh(),
    );
    assert_eq!(source.access_token().await.unwrap(), "AT-SECRET");
    assert_eq!(source.access_token().await.unwrap(), "AT-SECRET");
    assert_eq!(server.count("/token"), 1);

    // Errors never echo token material.
    let failing = spawn(|_| (400, r#"{"error":"invalid_grant"}"#.into())).await;
    let source = TokenSource::new(
        reqwest::Client::new(),
        &format!("{}/token", failing.base),
        OAuthClient {
            client_id: "cid".into(),
            client_secret: "s".into(),
        },
        "me@example.com",
        store_with_refresh(),
    );
    let err = format!("{:?}", source.access_token().await.unwrap_err());
    assert!(!err.contains("REFRESH-TOKEN"), "{err}");
}

#[tokio::test]
async fn consent_flow_uses_loopback_pkce_and_returns_the_refresh_token() {
    let server = spawn(|r: &Recorded| {
        assert_eq!(r.path, "/token");
        assert!(
            r.body.contains("grant_type=authorization_code"),
            "{}",
            r.body
        );
        assert!(r.body.contains("code=AUTHCODE"), "{}", r.body);
        (
            200,
            r#"{"access_token":"AT","expires_in":3600,"refresh_token":"RT"}"#.into(),
        )
    })
    .await;
    let endpoints = OAuthEndpoints {
        auth_url: "https://accounts.example/auth".into(),
        token_url: format!("{}/token", server.base),
    };
    let client = OAuthClient {
        client_id: "cid".into(),
        client_secret: "s".into(),
    };
    let seen_url = Arc::new(Mutex::new(String::new()));
    let seen = Arc::clone(&seen_url);
    let http = reqwest::Client::new();

    // The "browser": read the consent URL, then hit the redirect with a code and the state.
    let browser = |url: &str| {
        *seen.lock().unwrap() = url.to_owned();
        let parsed = reqwest::Url::parse(url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        let redirect = format!("{}/?code=AUTHCODE&state={}", q["redirect_uri"], q["state"]);
        tokio::spawn(async move {
            let body = reqwest::get(redirect).await.unwrap().text().await.unwrap();
            assert!(body.contains("authorization received"));
        });
    };
    let result = run_consent(&endpoints, &client, &http, browser, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(result.refresh_token, "RT");

    let url = reqwest::Url::parse(&seen_url.lock().unwrap()).unwrap();
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["scope"].split(' ').count(), 2);
    // The verifier sent to the token endpoint hashes to the challenge in the consent URL.
    let verifier = server
        .requests()
        .iter()
        .find_map(|r| {
            r.body
                .split('&')
                .find_map(|kv| kv.strip_prefix("code_verifier=").map(str::to_owned))
        })
        .unwrap();
    assert_eq!(pkce_challenge(&verifier), q["code_challenge"]);
}

#[tokio::test]
async fn consent_aborts_on_state_mismatch_and_on_timeout() {
    let endpoints = OAuthEndpoints {
        auth_url: "https://accounts.example/auth".into(),
        token_url: "http://127.0.0.1:1/token".into(),
    };
    let client = OAuthClient {
        client_id: "cid".into(),
        client_secret: "s".into(),
    };
    let http = reqwest::Client::new();

    let forged = |url: &str| {
        let q: std::collections::HashMap<_, _> = reqwest::Url::parse(url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        let redirect = format!("{}/?code=X&state=FORGED", q["redirect_uri"]);
        tokio::spawn(async move {
            let _ = reqwest::get(redirect).await;
        });
    };
    let r = run_consent(&endpoints, &client, &http, forged, Duration::from_secs(5)).await;
    assert!(matches!(r, Err(AuthError::StateMismatch)));

    let r = run_consent(
        &endpoints,
        &client,
        &http,
        |_| {},
        Duration::from_millis(200),
    )
    .await;
    assert!(matches!(r, Err(AuthError::Timeout)));
}
