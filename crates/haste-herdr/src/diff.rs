//! Turning snapshots and events into agent records and transitions. Pure.

use std::collections::BTreeSet;

use haste_core::herdr::{project_name, AgentStatus, AgentStatusRecord};

use crate::protocol::{Snapshot, StatusEvent};

/// A change in one agent's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub record: AgentStatusRecord,
    /// The earlier status, or `None` if the agent was not known before.
    pub from: Option<AgentStatus>,
}

/// What an event did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventOutcome {
    Changed(Transition),
    NoChange,
    /// The event names a pane or workspace we have no label for; fetch a fresh snapshot.
    NeedsSnapshot,
}

/// Every pane's workspace label, so the daemon can place a pane that has no detected agent.
#[must_use]
pub fn pane_labels(snapshot: &Snapshot) -> std::collections::HashMap<String, String> {
    snapshot
        .panes
        .iter()
        .filter_map(|p| {
            let w = snapshot
                .workspaces
                .iter()
                .find(|w| w.workspace_id == p.workspace_id)?;
            Some((p.pane_id.clone(), project_name(&w.label)))
        })
        .collect()
}

/// Workspace labels in a snapshot: the machine's project tags.
#[must_use]
pub fn labels(snapshot: &Snapshot) -> BTreeSet<String> {
    snapshot
        .workspaces
        .iter()
        .map(|w| project_name(&w.label))
        .collect()
}

/// Replace a machine's records with a snapshot. `status_since` is kept for agents whose status did
/// not change. With `baseline` set (the first snapshot after start or recovery) no transitions are
/// reported, so existing states are not announced.
#[must_use]
pub fn apply_snapshot(
    machine: &str,
    prior: &[AgentStatusRecord],
    snapshot: &Snapshot,
    now: i64,
    baseline: bool,
) -> (Vec<AgentStatusRecord>, Vec<Transition>) {
    let mut records = Vec::new();
    let mut transitions = Vec::new();
    for agent in &snapshot.agents {
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|w| w.workspace_id == agent.workspace_id)
        else {
            continue; // an agent in an unknown workspace cannot be tagged; skip it
        };
        let before = prior
            .iter()
            .find(|p| p.pane_id == agent.pane_id && p.workspace_id == agent.workspace_id);
        let status_since = match before {
            Some(p) if p.status == agent.agent_status => p.status_since,
            _ => now,
        };
        let record = AgentStatusRecord {
            machine: machine.to_owned(),
            workspace_id: agent.workspace_id.clone(),
            workspace_label: project_name(&workspace.label),
            pane_id: agent.pane_id.clone(),
            agent: agent.agent.clone(),
            status: agent.agent_status,
            status_since,
        };
        if !baseline && before.map(|p| p.status) != Some(agent.agent_status) {
            transitions.push(Transition {
                record: record.clone(),
                from: before.map(|p| p.status),
            });
        }
        records.push(record);
    }
    (records, transitions)
}

/// Apply one `pane.agent_status_changed` event to a machine's records.
pub fn apply_event(
    machine: &str,
    records: &mut Vec<AgentStatusRecord>,
    event: &StatusEvent,
    now: i64,
) -> EventOutcome {
    if let Some(r) = records
        .iter_mut()
        .find(|r| r.pane_id == event.pane_id && r.workspace_id == event.workspace_id)
    {
        if r.status == event.agent_status {
            return EventOutcome::NoChange;
        }
        let from = r.status;
        r.status = event.agent_status;
        r.status_since = now;
        if event.agent.is_some() {
            r.agent.clone_from(&event.agent);
        }
        return EventOutcome::Changed(Transition {
            record: r.clone(),
            from: Some(from),
        });
    }
    // A new pane: we can place it only if we already know its workspace's label.
    let Some(label) = records
        .iter()
        .find(|r| r.workspace_id == event.workspace_id)
        .map(|r| r.workspace_label.clone())
    else {
        return EventOutcome::NeedsSnapshot;
    };
    let record = AgentStatusRecord {
        machine: machine.to_owned(),
        workspace_id: event.workspace_id.clone(),
        workspace_label: label,
        pane_id: event.pane_id.clone(),
        agent: event.agent.clone(),
        status: event.agent_status,
        status_since: now,
    };
    records.push(record.clone());
    EventOutcome::Changed(Transition { record, from: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AgentEntry, Workspace};

    fn snap(agents: &[(&str, &str, AgentStatus)]) -> Snapshot {
        Snapshot {
            protocol: 22,
            workspaces: vec![
                Workspace {
                    workspace_id: "w1".into(),
                    label: "haste".into(),
                },
                Workspace {
                    workspace_id: "w2".into(),
                    label: "notes".into(),
                },
            ],
            panes: vec![],
            agents: agents
                .iter()
                .map(|(p, w, s)| AgentEntry {
                    pane_id: (*p).into(),
                    workspace_id: (*w).into(),
                    agent_status: *s,
                    agent: None,
                })
                .collect(),
        }
    }

    #[test]
    fn records_carry_the_workspace_label_and_nothing_else() {
        let (records, _) = apply_snapshot(
            "m1",
            &[],
            &snap(&[("w1:p1", "w1", AgentStatus::Working)]),
            100,
            true,
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].workspace_label, "haste");
        assert_eq!(records[0].machine, "m1");
        assert_eq!(records[0].status_since, 100);
    }

    #[test]
    fn the_baseline_snapshot_reports_no_transitions() {
        let (_, t) = apply_snapshot(
            "m1",
            &[],
            &snap(&[("w1:p1", "w1", AgentStatus::Blocked)]),
            1,
            true,
        );
        assert!(t.is_empty());
    }

    #[test]
    fn a_later_snapshot_diffs_into_transitions_and_keeps_status_since() {
        let first = apply_snapshot(
            "m1",
            &[],
            &snap(&[
                ("w1:p1", "w1", AgentStatus::Working),
                ("w2:p1", "w2", AgentStatus::Idle),
            ]),
            100,
            true,
        )
        .0;
        let (second, t) = apply_snapshot(
            "m1",
            &first,
            &snap(&[
                ("w1:p1", "w1", AgentStatus::Blocked),
                ("w2:p1", "w2", AgentStatus::Idle),
            ]),
            200,
            false,
        );
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].from, Some(AgentStatus::Working));
        assert_eq!(t[0].record.status, AgentStatus::Blocked);
        let idle = second.iter().find(|r| r.pane_id == "w2:p1").unwrap();
        assert_eq!(
            idle.status_since, 100,
            "unchanged agents keep their time in status"
        );
        assert_eq!(
            second
                .iter()
                .find(|r| r.pane_id == "w1:p1")
                .unwrap()
                .status_since,
            200
        );
    }

    #[test]
    fn a_snapshot_replaces_stale_blocked_state() {
        let blocked = apply_snapshot(
            "m1",
            &[],
            &snap(&[("w1:p1", "w1", AgentStatus::Blocked)]),
            1,
            true,
        )
        .0;
        let (after, _) = apply_snapshot(
            "m1",
            &blocked,
            &snap(&[("w1:p1", "w1", AgentStatus::Working)]),
            9,
            false,
        );
        assert!(after.iter().all(|r| r.status != AgentStatus::Blocked));
        let (gone, _) = apply_snapshot("m1", &blocked, &snap(&[]), 9, false);
        assert!(gone.is_empty(), "closed panes disappear");
    }

    #[test]
    fn events_update_known_panes_and_ask_for_a_snapshot_for_unknown_workspaces() {
        let mut records = apply_snapshot(
            "m1",
            &[],
            &snap(&[("w1:p1", "w1", AgentStatus::Working)]),
            1,
            true,
        )
        .0;
        let ev = |pane: &str, ws: &str, s| StatusEvent {
            pane_id: pane.into(),
            workspace_id: ws.into(),
            agent_status: s,
            agent: None,
        };
        assert!(matches!(
            apply_event("m1", &mut records, &ev("w1:p1", "w1", AgentStatus::Blocked), 5),
            EventOutcome::Changed(t) if t.from == Some(AgentStatus::Working)
        ));
        assert_eq!(
            apply_event(
                "m1",
                &mut records,
                &ev("w1:p1", "w1", AgentStatus::Blocked),
                6
            ),
            EventOutcome::NoChange
        );
        // a new pane in a known workspace is added with that workspace's label
        match apply_event("m1", &mut records, &ev("w1:p2", "w1", AgentStatus::Idle), 7) {
            EventOutcome::Changed(t) => {
                assert_eq!((t.record.workspace_label.as_str(), t.from), ("haste", None))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            apply_event("m1", &mut records, &ev("w9:p1", "w9", AgentStatus::Idle), 8),
            EventOutcome::NeedsSnapshot
        );
    }

    #[test]
    fn panes_without_an_agent_still_map_to_their_workspace_label() {
        let mut s = snap(&[]);
        s.panes = vec![
            crate::protocol::PaneEntry {
                pane_id: "w1:p1".into(),
                workspace_id: "w1".into(),
            },
            crate::protocol::PaneEntry {
                pane_id: "w2:p4".into(),
                workspace_id: "w2".into(),
            },
            crate::protocol::PaneEntry {
                pane_id: "w9:p1".into(),
                workspace_id: "w9".into(),
            },
        ];
        let m = pane_labels(&s);
        assert_eq!(m.get("w1:p1").map(String::as_str), Some("haste"));
        assert_eq!(m.get("w2:p4").map(String::as_str), Some("notes"));
        assert!(
            !m.contains_key("w9:p1"),
            "a pane in an unknown workspace has no label"
        );
    }

    #[test]
    fn herdr_labels_with_a_position_prefix_become_plain_project_names() {
        let mut s = snap(&[("w1:p1", "w1", AgentStatus::Idle)]);
        s.workspaces[0].label = "[4] haste".into();
        s.workspaces[1].label = "[2] notes".into();
        s.panes = vec![crate::protocol::PaneEntry {
            pane_id: "w1:p1".into(),
            workspace_id: "w1".into(),
        }];
        let (records, _) = apply_snapshot("m1", &[], &s, 1, true);
        assert_eq!(records[0].workspace_label, "haste");
        assert_eq!(
            pane_labels(&s).get("w1:p1").map(String::as_str),
            Some("haste")
        );
        assert_eq!(
            labels(&s).into_iter().collect::<Vec<_>>(),
            ["haste", "notes"]
        );
    }

    #[test]
    fn labels_are_the_workspace_names() {
        let l = labels(&snap(&[]));
        assert_eq!(l.into_iter().collect::<Vec<_>>(), ["haste", "notes"]);
    }
}
