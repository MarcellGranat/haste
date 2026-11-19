//! One line for Herdr's status bar: the task that is overdue, whether mail has arrived, and
//! whether a connection has a problem. Plain text only (Herdr strips colours), and nothing at
//! all when there is nothing to say, so the entry disappears.
//!
//! `haste-tui --status` prints it; Herdr runs that on an interval (see the README).

use crate::app::App;
use crate::inbox;

/// A connection counts as stuck when its last successful read is older than this (a poll is
/// every five minutes).
const STALE_SECS: i64 = 15 * 60;
/// The longest an overdue task's title is shown, in characters.
const TITLE_MAX: usize = 28;
/// How many connections are named before the rest are counted.
const PROBLEMS_NAMED: usize = 2;

/// The segments, from an App filled with the threads, rules, tasks and connectors.
#[must_use]
pub fn line(app: &App, now: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(task) = overdue(app, now) {
        parts.push(task);
    }
    let unread = unread_mail(app);
    if unread > 0 {
        parts.push(format!("✉ {unread}"));
    }
    if let Some(problems) = problems(app, now) {
        parts.push(problems);
    }
    parts.join("  ")
}

/// The most important overdue task (the highest priority, then the longest overdue), with how
/// many more there are.
fn overdue(app: &App, now: i64) -> Option<String> {
    let mut late: Vec<_> = app
        .reminder_list
        .iter()
        .filter(|r| {
            !r.reminder.completed
                && !r.set_aside
                && !r.snoozed
                && r.reminder.due_at.is_some_and(|due| due < now)
        })
        .collect();
    late.sort_by_key(|r| {
        (
            std::cmp::Reverse(r.priority.rank()),
            r.reminder.due_at.unwrap_or(i64::MAX),
        )
    });
    let first = late.first()?;
    let title: String = inbox::sanitize(&first.reminder.title)
        .replace('\n', " ")
        .chars()
        .take(TITLE_MAX)
        .collect();
    let more = late.len() - 1;
    Some(if more > 0 {
        format!("☐ {title} +{more}")
    } else {
        format!("☐ {title}")
    })
}

/// Unread threads that are in the inbox: not archived, set aside, snoozed, muted or from a
/// sender not accepted yet.
fn unread_mail(app: &App) -> usize {
    app.threads
        .iter()
        .filter(|t| {
            t.unread
                && !t.triage.archived
                && !t.triage.set_aside
                && !app.is_snoozed(t)
                && !app.is_blocked(t)
        })
        .count()
}

/// The connections that are stopped or have not been read lately, by name.
fn problems(app: &App, now: i64) -> Option<String> {
    let bad: Vec<&str> = app
        .connectors
        .iter()
        .filter(|c| {
            c.engaged || (c.registered && c.last_synced.is_some_and(|at| now - at > STALE_SECS))
        })
        .map(|c| c.source.as_str())
        .collect();
    bad.first()?;
    let named: Vec<&str> = bad.iter().take(PROBLEMS_NAMED).copied().collect();
    let more = bad.len().saturating_sub(PROBLEMS_NAMED);
    Some(if more > 0 {
        format!("⚠ {} +{more}", named.join(" "))
    } else {
        format!("⚠ {}", named.join(" "))
    })
}

/// What Herdr shows when the daemon cannot be reached.
#[must_use]
pub fn unreachable() -> String {
    "⚠ haste".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use haste_core::model::{Reminder, TaskPriority};
    use haste_proto::{ConnectorView, ReminderView};

    const NOW: i64 = 1_800_000_000;

    fn task(title: &str, due: Option<i64>, priority: TaskPriority) -> ReminderView {
        ReminderView {
            reminder: Reminder {
                id: title.into(),
                external_id: title.into(),
                title: title.into(),
                notes_snippet: String::new(),
                due_at: due,
                completed: false,
                list_name: "l".into(),
                remote_modified_at: 0,
                missing_since: None,
                version: 1,
            },
            tags: vec![],
            repeat: None,
            minutes: 30,
            priority,
            early: None,
            set_aside: false,
            snoozed: false,
        }
    }

    fn connector(source: &str, engaged: bool, last_synced: Option<i64>) -> ConnectorView {
        ConnectorView {
            source: source.into(),
            engaged,
            registered: true,
            last_synced,
        }
    }

    #[test]
    fn nothing_to_say_is_an_empty_line() {
        let mut app = App::new();
        app.set_reminder_list(vec![task("later", Some(NOW + 3_600), TaskPriority::Middle)]);
        app.set_connectors(vec![connector("gmail", false, Some(NOW - 60))]);
        assert_eq!(line(&app, NOW), "");
    }

    #[test]
    fn the_most_important_overdue_task_is_named_and_the_others_counted() {
        let mut app = App::new();
        let mut aside = task("aside", Some(NOW - 9_000), TaskPriority::Urgent);
        aside.set_aside = true;
        let mut done = task("done", Some(NOW - 9_000), TaskPriority::Urgent);
        done.reminder.completed = true;
        app.set_reminder_list(vec![
            task("old", Some(NOW - 5_000), TaskPriority::Middle),
            task("urgent", Some(NOW - 100), TaskPriority::Urgent),
            task("older", Some(NOW - 8_000), TaskPriority::Middle),
            task("undated", None, TaskPriority::Urgent),
            task("future", Some(NOW + 60), TaskPriority::Urgent),
            aside,
            done,
        ]);
        assert_eq!(line(&app, NOW), "☐ urgent +2");
        // a long title is cut
        app.set_reminder_list(vec![task(
            &"x".repeat(80),
            Some(NOW - 1),
            TaskPriority::Middle,
        )]);
        assert_eq!(line(&app, NOW), format!("☐ {}", "x".repeat(TITLE_MAX)));
    }

    #[test]
    fn a_stopped_or_stale_connection_is_named() {
        let mut app = App::new();
        app.set_connectors(vec![
            connector("gmail", false, Some(NOW - 60)),
            connector("icloud", true, Some(NOW - 60)),
            connector("whatsapp", false, Some(NOW - 3_600)),
            connector("messenger", false, None),
            connector("gcal", true, None),
        ]);
        assert_eq!(line(&app, NOW), "⚠ icloud whatsapp +1");
        // a connection not read since the start is not a problem yet
        app.set_connectors(vec![connector("gmail", false, None)]);
        assert_eq!(line(&app, NOW), "");
        assert_eq!(unreachable(), "⚠ haste");
    }

    #[test]
    fn the_parts_come_in_a_fixed_order_with_two_spaces_between() {
        let mut app = App::new();
        app.set_reminder_list(vec![task("rent", Some(NOW - 10), TaskPriority::Middle)]);
        app.set_connectors(vec![connector("icloud", true, None)]);
        assert_eq!(line(&app, NOW), "☐ rent  ⚠ icloud");
    }

    fn thread(id: &str, unread: bool, known: bool) -> haste_proto::ThreadView {
        haste_proto::ThreadView {
            thread: haste_core::model::Thread {
                id: id.into(),
                source: haste_core::model::Source::Gmail,
                external_id: id.into(),
                subject: format!("s-{id}"),
                participants: vec![],
                last_activity_at: 1,
                version: 1,
            },
            triage: haste_core::model::Triage::new(id, haste_core::model::ObjectKind::Thread),
            tags: vec![],
            last: None,
            unread,
            replied: false,
            sender_known: known,
            sender: Some(format!("{id}@x.org")),
        }
    }

    #[test]
    fn only_unread_mail_in_the_inbox_counts_as_new() {
        let mut app = App::new();
        let mut archived = thread("archived", true, true);
        archived.triage.archived = true;
        let mut aside = thread("aside", true, true);
        aside.triage.set_aside = true;
        app.set_threads(vec![
            thread("new1", true, true),
            thread("new2", true, true),
            thread("read", false, true),
            thread("stranger", true, false),
            thread("noisy", true, true),
            archived,
            aside,
        ]);
        app.set_ignore(&["^noisy@x\\.org$".to_owned()], vec![]);
        assert_eq!(line(&app, NOW), "✉ 2");
    }
}
