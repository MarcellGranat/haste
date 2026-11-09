//! Everything haste sends or reads must exist in the committed copy of herdr's exported schema.
//! When herdr is upgraded, refresh the file with
//! `herdr api schema --json > crates/haste-herdr/schema/herdr-api-22.json` (rename for a new
//! protocol), review the diff, and bump `HERDR_PROTOCOL`.

use std::collections::BTreeSet;

use haste_herdr::local::{subscriptions_for, GLOBAL_SUBSCRIPTIONS, STATUS_SUBSCRIPTION};
use haste_herdr::HERDR_PROTOCOL;
use serde_json::Value;

fn schema() -> Value {
    let text = include_str!("../schema/herdr-api-22.json");
    serde_json::from_str(text).expect("committed schema is valid JSON")
}

fn consts(one_of: &Value, prop: &str) -> BTreeSet<String> {
    one_of
        .as_array()
        .expect("oneOf array")
        .iter()
        .filter_map(|o| o["properties"][prop]["const"].as_str().map(str::to_owned))
        .collect()
}

fn required(v: &Value) -> BTreeSet<String> {
    v["required"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn props(v: &Value) -> BTreeSet<String> {
    v["properties"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn the_committed_schema_is_for_the_pinned_protocol() {
    assert_eq!(
        schema()["protocol"].as_u64(),
        Some(u64::from(HERDR_PROTOCOL))
    );
}

#[test]
fn every_request_method_haste_uses_exists() {
    let s = schema();
    let methods = consts(&s["schemas"]["request"]["oneOf"], "method");
    for used in ["session.snapshot", "events.subscribe", "agent.prompt"] {
        assert!(
            methods.contains(used),
            "method {used} missing from the schema"
        );
    }
    // agent.prompt takes exactly the fields we send
    let prompt = &s["schemas"]["request"]["$defs"]["AgentPromptParams"];
    assert!(required(prompt).is_superset(&BTreeSet::from(["target".to_owned(), "text".to_owned()])));
}

#[test]
fn every_subscription_haste_sends_satisfies_the_schema_and_none_reads_output() {
    let s = schema();
    let entries = s["schemas"]["request"]["$defs"]["Subscription"]["oneOf"]
        .as_array()
        .unwrap();
    let panes: BTreeSet<String> = ["w1:p1", "w1:p2", "w2:p1"]
        .iter()
        .map(|p| (*p).to_owned())
        .collect();
    let sent = subscriptions_for(&panes);
    assert_eq!(
        sent.len(),
        GLOBAL_SUBSCRIPTIONS.len() + panes.len(),
        "one status subscription per pane"
    );
    for sub in &sent {
        let ty = sub["type"].as_str().unwrap();
        let entry = entries
            .iter()
            .find(|e| e["properties"]["type"]["const"].as_str() == Some(ty))
            .unwrap_or_else(|| panic!("subscription {ty} is not in the schema"));
        // every field the schema requires for this type must be present, and nothing unknown
        for field in required(entry) {
            assert!(
                sub.get(&field).is_some(),
                "{ty} requires `{field}` (real herdr rejects it otherwise): {sub}"
            );
        }
        let allowed = props(entry);
        for key in sub.as_object().unwrap().keys() {
            assert!(
                allowed.contains(key),
                "{ty} has no field `{key}` in the schema"
            );
        }
        assert!(
            !ty.contains("output") && !ty.contains("scroll"),
            "{ty} would carry pane content"
        );
    }
    // the status subscription really is per pane in this protocol
    let status = entries
        .iter()
        .find(|e| e["properties"]["type"]["const"].as_str() == Some(STATUS_SUBSCRIPTION))
        .unwrap();
    assert!(required(status).contains("pane_id"));
}

#[test]
fn the_status_event_has_the_fields_haste_reads() {
    let s = schema();
    let event = &s["schemas"]["subscription_event"]["$defs"]["PaneAgentStatusChangedEvent"];
    for field in ["pane_id", "workspace_id", "agent_status"] {
        assert!(
            required(event).contains(field),
            "{field} is no longer a required event field"
        );
    }
    let kinds: Vec<&str> = s["schemas"]["subscription_event"]["$defs"]["SubscriptionEventKind"]
        ["enum"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(kinds.contains(&"pane.agent_status_changed"));
}

#[test]
fn agent_status_values_match_what_haste_understands() {
    let s = schema();
    let values: BTreeSet<String> = s["schemas"]["subscription_event"]["$defs"]["AgentStatus"]
        ["enum"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let ours: BTreeSet<String> = ["idle", "working", "blocked", "done", "unknown"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(values, ours);
}

#[test]
fn the_snapshot_has_the_workspace_and_agent_fields_haste_reads() {
    let s = schema();
    let defs = &s["schemas"]["success_response"]["$defs"];
    let result_types = consts(&defs["ResponseResult"]["oneOf"], "type");
    for used in ["session_snapshot", "subscription_started", "agent_prompted"] {
        assert!(result_types.contains(used), "result {used} missing");
    }
    let snapshot = &defs["SessionSnapshot"];
    for field in ["protocol", "workspaces", "agents"] {
        assert!(props(snapshot).contains(field), "snapshot lost {field}");
    }
    // workspace and agent records: the fields we deserialise
    let find = |name: &str| -> &Value { &defs[name] };
    let workspace = ["WorkspaceInfo", "WorkspaceSnapshot", "SnapshotWorkspace"]
        .iter()
        .map(|n| find(n))
        .find(|v| !v.is_null());
    let agent = ["AgentInfo", "AgentSnapshot", "SnapshotAgent"]
        .iter()
        .map(|n| find(n))
        .find(|v| !v.is_null());
    let workspace = workspace.expect("a workspace record type");
    assert!(
        props(workspace).is_superset(&BTreeSet::from([
            "workspace_id".to_owned(),
            "label".to_owned()
        ])),
        "{:?}",
        props(workspace)
    );
    let agent = agent.expect("an agent record type");
    for field in ["pane_id", "workspace_id", "agent_status"] {
        assert!(props(agent).contains(field), "agent record lost {field}");
    }
}
