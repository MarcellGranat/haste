//! Wire shapes of the Beeper Desktop API, confirmed on 2026-11-10 against a live run and the
//! local OpenAPI spec the app serves at `/v1/spec` (see the change's design.md, Spike results).
//! Everything that depends on a name lives here, with tolerant aliases for older shapes, so a
//! change is a one-file edit. A missing required field fails closed (the parse errors) instead of
//! guessing.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct WireAccount {
    #[serde(alias = "accountID", alias = "accountId")]
    pub id: String,
    /// The network name, for example `whatsapp`, `facebookgo`, `imessage` (exact strings: spike S3).
    #[serde(default)]
    pub network: String,
    /// Whether the connection runs on-device or in Beeper Cloud, if the API says (spike S4).
    #[serde(
        default,
        alias = "connectionType",
        alias = "connection",
        alias = "bridgeType"
    )]
    pub connection: Option<String>,
    /// The bridge object as Beeper reports it (spike 2026-11-10: present on every account; its
    /// keys decide whether it can tell on-device from cloud). Kept raw, never stored.
    #[serde(default)]
    pub bridge: Option<Value>,
}
impl WireAccount {
    /// Where the account runs, as Beeper reports it. Spike 2026-11-10 and the local OpenAPI spec:
    /// `bridge.provider` is one of `cloud`, `self-hosted`, `local`, `platform-sdk` (Beeper Desktop
    /// 4.2.785 and later). Older shapes fall back to a flat connection field.
    #[must_use]
    pub fn provider(&self) -> Option<String> {
        self.bridge
            .as_ref()
            .and_then(|b| b.get("provider"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| self.connection.clone())
    }

    /// The underlying bridge type (`whatsapp`, ...), a steadier network key than the display name.
    #[must_use]
    pub fn bridge_type(&self) -> Option<String> {
        self.bridge
            .as_ref()
            .and_then(|b| b.get("type"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireChat {
    #[serde(alias = "chatID", alias = "chatId")]
    pub id: String,
    #[serde(default, alias = "accountID", alias = "accountId")]
    pub account_id: String,
    #[serde(default, alias = "name")]
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireMessage {
    #[serde(alias = "messageID", alias = "messageId")]
    pub id: String,
    #[serde(default, alias = "chatID", alias = "chatId")]
    pub chat_id: String,
    #[serde(default, alias = "senderID", alias = "senderId")]
    pub sender_id: String,
    #[serde(default)]
    pub text: Option<String>,
    /// RFC 3339.
    #[serde(default)]
    pub timestamp: String,
    /// Orders messages within a chat; a string or a number (spike S5).
    #[serde(default, alias = "sortKey")]
    pub sort_key: Option<Value>,
    #[serde(default, alias = "type", alias = "kind")]
    pub kind: Option<String>,
    #[serde(default, alias = "isSender", alias = "fromMe")]
    pub is_sender: bool,
    /// Spike 2026-11-10: messages carry `isDeleted` and `isHidden`; both are skipped.
    #[serde(default, rename = "isDeleted")]
    pub is_deleted: bool,
    #[serde(default, rename = "isHidden")]
    pub is_hidden: bool,
}

/// A list answer: `{items, hasMore, cursor}` or a bare array.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum List<T> {
    Page {
        items: Vec<T>,
        #[serde(default, alias = "hasMore")]
        has_more: bool,
        #[serde(default, alias = "nextCursor", alias = "oldestCursor")]
        cursor: Option<String>,
    },
    Bare(Vec<T>),
}

impl<T> List<T> {
    #[must_use]
    pub fn into_parts(self) -> (Vec<T>, bool, Option<String>) {
        match self {
            Self::Page {
                items,
                has_more,
                cursor,
            } => (items, has_more, cursor),
            Self::Bare(items) => (items, false, None),
        }
    }
}

/// The answer to a send: the id of the message that was created.
#[derive(Debug, Deserialize)]
pub struct WireSent {
    #[serde(
        default,
        alias = "messageID",
        alias = "messageId",
        alias = "pendingMessageID"
    )]
    pub id: Option<String>,
}

/// Compare two sort keys (strings or numbers). All-digit keys compare as numbers, anything else
/// lexicographically.
#[must_use]
pub fn sort_key_string(key: &Value) -> Option<String> {
    match key {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[must_use]
pub fn key_newer(candidate: &str, than: &str) -> bool {
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if numeric(candidate) && numeric(than) {
        (candidate.len(), candidate) > (than.len(), than)
    } else {
        candidate > than
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_and_both_list_shapes_parse() {
        let a: WireAccount = serde_json::from_str(
            r#"{"accountID":"a1","network":"whatsapp","connectionType":"on-device"}"#,
        )
        .unwrap();
        assert_eq!(
            (a.id.as_str(), a.connection.as_deref()),
            ("a1", Some("on-device"))
        );
        let page: List<WireChat> = serde_json::from_str(r#"{"items":[{"chatID":"c1","accountID":"a1","name":"X"}],"hasMore":true,"nextCursor":"n"}"#).unwrap();
        let (items, more, cursor) = page.into_parts();
        assert_eq!(
            (
                items[0].id.as_str(),
                items[0].title.as_str(),
                more,
                cursor.as_deref()
            ),
            ("c1", "X", true, Some("n"))
        );
        let bare: List<WireChat> = serde_json::from_str(r#"[{"id":"c2"}]"#).unwrap();
        assert_eq!(bare.into_parts().0[0].id, "c2");
    }

    #[test]
    fn a_missing_required_field_fails_closed() {
        assert!(serde_json::from_str::<WireChat>(r#"{"title":"no id"}"#).is_err());
        assert!(serde_json::from_str::<WireMessage>(r#"{"text":"no id"}"#).is_err());
    }

    #[test]
    fn sort_keys_compare_numerically_when_numeric_and_lexicographically_otherwise() {
        assert!(key_newer("10", "9"), "numeric, not lexicographic");
        assert!(key_newer("100", "99"));
        assert!(!key_newer("9", "10"));
        assert!(key_newer("b", "a") && !key_newer("a", "b") && !key_newer("a", "a"));
        assert_eq!(sort_key_string(&serde_json::json!(42)), Some("42".into()));
        assert_eq!(sort_key_string(&serde_json::json!("k")), Some("k".into()));
        assert_eq!(sort_key_string(&serde_json::json!(null)), None);
    }
}
