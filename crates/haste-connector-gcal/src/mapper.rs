//! Mapping of Google events to the `Event` record, and of proposals to request bodies.

use haste_core::connector::{EventPatch, NewEvent};
use haste_core::model::{description_snippet, Event};
use serde_json::{json, Value};

use crate::client::{ApiEvent, ApiTime};
use crate::consts::{CAL_BACKFILL_DAYS, CAL_LOOKAHEAD_DAYS, DAY_SECS};
use crate::timefmt::{format_date, format_rfc3339, parse_date, parse_rfc3339};

fn time_of(t: &Option<ApiTime>) -> Option<(i64, bool)> {
    let t = t.as_ref()?;
    if let Some(dt) = &t.date_time {
        return Some((parse_rfc3339(dt)?, false));
    }
    Some((parse_date(t.date.as_deref()?)?, true))
}

/// Map one API event. A cancelled event from a sync listing carries no times, so it maps to
/// zeros (it is only used to hide the stored row).
#[must_use]
pub fn map_event(calendar: &str, api: &ApiEvent) -> Event {
    let (start, all_day) = time_of(&api.start).unwrap_or((0, false));
    let (end, _) = time_of(&api.end).unwrap_or((start, all_day));
    let mut attendees: Vec<String> = api
        .attendees
        .iter()
        .map(|a| a.email.to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    attendees.sort();
    attendees.dedup();
    Event {
        id: String::new(),
        external_id: format!("{calendar}/{}", api.id),
        calendar: calendar.to_owned(),
        title: api.summary.clone().unwrap_or_default(),
        start_at: start,
        end_at: end,
        all_day,
        status: if api.status.is_empty() {
            "confirmed".into()
        } else {
            api.status.clone()
        },
        attendees,
        location: api.location.clone().unwrap_or_default(),
        description_snippet: description_snippet(api.description.as_deref().unwrap_or("")),
        updated_at: api
            .updated
            .as_deref()
            .and_then(parse_rfc3339)
            .unwrap_or_default(),
        missing_since: None,
        version: 0,
    }
}

/// Whether an event is stored: inside the window, a recurring master, or a cancellation (which
/// must reach the store to hide an event it already holds).
#[must_use]
pub fn keep(api: &ApiEvent, event: &Event, now: i64) -> bool {
    if event.status == "cancelled" || !api.recurrence.is_empty() {
        return true;
    }
    event.end_at >= now - CAL_BACKFILL_DAYS * DAY_SECS
        && event.start_at <= now + CAL_LOOKAHEAD_DAYS * DAY_SECS
}

fn when(t: i64, all_day: bool) -> Value {
    if all_day {
        json!({ "date": format_date(t) })
    } else {
        json!({ "dateTime": format_rfc3339(t) })
    }
}

/// The `events.insert` body.
#[must_use]
pub fn insert_body(event: &NewEvent) -> Value {
    let mut body = json!({
        "summary": event.title,
        "start": when(event.start, event.all_day),
        "end": when(event.end, event.all_day),
    });
    if let Some(d) = &event.description {
        body["description"] = json!(d);
    }
    if !event.attendees.is_empty() {
        body["attendees"] = event
            .attendees
            .iter()
            .map(|e| json!({ "email": e }))
            .collect();
    }
    body
}

/// The `events.patch` body: only the allow-listed fields that are set. There is no attendee or
/// status key, by construction.
#[must_use]
pub fn patch_body(patch: &EventPatch) -> Value {
    let mut body = serde_json::Map::new();
    if let Some(t) = &patch.title {
        body.insert("summary".into(), json!(t));
    }
    if let Some(s) = patch.start {
        body.insert("start".into(), when(s, false));
    }
    if let Some(e) = patch.end {
        body.insert("end".into(), when(e, false));
    }
    if let Some(d) = &patch.description {
        body.insert("description".into(), json!(d));
    }
    if let Some(l) = &patch.location {
        body.insert("location".into(), json!(l));
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(json: &str) -> ApiEvent {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_timed_event_maps_with_lowercased_unique_attendees() {
        let e = map_event(
            "primary",
            &api(
                r#"{"id":"e1","status":"confirmed","summary":"Plan","location":"Room","description":"d",
              "start":{"dateTime":"2026-11-09T10:00:00+01:00"},"end":{"dateTime":"2026-11-09T11:00:00+01:00"},
              "attendees":[{"email":"Ana@X.org"},{"email":"ana@x.org"},{"email":"b@x.org"}],
              "updated":"2026-11-01T00:00:00.000Z"}"#,
            ),
        );
        assert_eq!(e.external_id, "primary/e1");
        assert_eq!(
            (e.start_at, e.end_at, e.all_day),
            (1_794_218_400 - 3600, 1_794_218_400, false)
        );
        assert_eq!(e.attendees, ["ana@x.org", "b@x.org"]);
        assert!(e.location_present());
    }

    #[test]
    fn an_all_day_event_uses_utc_midnight() {
        let e = map_event(
            "c",
            &api(r#"{"id":"d","start":{"date":"2026-11-09"},"end":{"date":"2026-11-10"}}"#),
        );
        assert!(e.all_day);
        assert_eq!(e.end_at - e.start_at, DAY_SECS);
        assert_eq!(e.status, "confirmed");
    }

    #[test]
    fn a_cancelled_stub_maps_to_zeros_and_is_always_kept() {
        let a = api(r#"{"id":"x","status":"cancelled"}"#);
        let e = map_event("c", &a);
        assert_eq!((e.start_at, e.end_at), (0, 0));
        assert!(keep(&a, &e, 1_794_218_400));
    }

    #[test]
    fn the_window_keeps_near_events_and_recurring_masters_only() {
        let now = 1_794_218_400;
        let mk = |start: i64, end: i64, rec: bool| {
            let mut a = api(r#"{"id":"x"}"#);
            if rec {
                a.recurrence = vec!["RRULE:FREQ=WEEKLY".into()];
            }
            let mut e = map_event("c", &a);
            (e.start_at, e.end_at) = (start, end);
            (a, e)
        };
        let day = DAY_SECS;
        for (start, end, rec, expect) in [
            (now, now + 3600, false, true),
            (now - 10 * day, now - 10 * day + 3600, false, true),
            (now - 400 * day, now - 400 * day + 3600, false, false),
            (now + 400 * day, now + 400 * day + 3600, false, false),
            (now - 400 * day, now - 400 * day + 3600, true, true),
        ] {
            let (a, e) = mk(start, end, rec);
            assert_eq!(keep(&a, &e, now), expect, "{start} {rec}");
        }
    }

    #[test]
    fn write_bodies_carry_only_allowed_fields() {
        let body = patch_body(&EventPatch {
            title: Some("T".into()),
            location: Some("L".into()),
            ..EventPatch::default()
        });
        let keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["location", "summary"]);
        let create = insert_body(&NewEvent {
            calendar: "c".into(),
            title: "T".into(),
            start: 0,
            end: 3600,
            all_day: false,
            attendees: vec!["a@x.org".into()],
            description: Some("d".into()),
        });
        assert_eq!(create["start"]["dateTime"], "1970-01-01T00:00:00Z");
        assert_eq!(create["attendees"][0]["email"], "a@x.org");
    }
}
