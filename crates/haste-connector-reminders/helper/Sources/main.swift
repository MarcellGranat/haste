// haste-reminders-helper: a tiny EventKit bridge. One JSON request on stdin, one JSON object on
// stdout. The request set is closed: authorization, list_lists, fetch_changed, create_reminder,
// complete_reminder, update_reminder. There is deliberately no delete.
import EventKit
import Foundation

let notesLimit = 1000  // the daemon cuts notes further; this only bounds the pipe

func emit(_ object: [String: Any]) -> Never {
    let data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data()
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
    exit(0)
}

func fail(_ kind: String, _ message: String = "") -> Never {
    emit(["op": "error", "kind": kind, "message": message])
}

func statusName(_ status: EKAuthorizationStatus) -> String {
    switch status {
    case .fullAccess: return "granted"
    case .notDetermined: return "not_determined"
    case .restricted: return "restricted"
    default: return "denied"  // denied, writeOnly, legacy authorized
    }
}

func authorized() -> Bool {
    EKEventStore.authorizationStatus(for: .reminder) == .fullAccess
}

func wire(_ r: EKReminder) -> [String: Any] {
    var out: [String: Any] = [
        "external_id": r.calendarItemIdentifier,
        "title": r.title ?? "",
        "notes": String((r.notes ?? "").prefix(notesLimit)),
        "completed": r.isCompleted,
        "list": r.calendar?.title ?? "",
        "modified": Int((r.lastModifiedDate ?? Date(timeIntervalSince1970: 0)).timeIntervalSince1970),
    ]
    if let comps = r.dueDateComponents, let date = Calendar.current.date(from: comps) {
        out["due"] = Int(date.timeIntervalSince1970)
    }
    if let rule = r.recurrenceRules?.first {
        let freq: String?
        switch rule.frequency {
        case .daily: freq = "day"
        case .weekly: freq = "week"
        case .monthly: freq = "month"
        case .yearly: freq = "year"
        @unknown default: freq = nil
        }
        if let freq = freq { out["repeat"] = ["freq": freq, "every": max(rule.interval, 1)] }
    }
    return out
}

// `--check-fixture <file>`: validate the shared protocol fixture so schema drift fails the build.
func checkFixture(_ path: String) -> Never {
    let required: [String: [String]] = [
        "authorization": ["prompt"], "list_lists": [], "fetch_changed": ["since", "offset", "limit", "include_completed"],
        "create_reminder": ["title"], "complete_reminder": ["external_id"], "update_reminder": ["external_id", "title"],
    ]
    guard let data = FileManager.default.contents(atPath: path),
        let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
        let requests = root["requests"] as? [[String: Any]]
    else { FileHandle.standardError.write(Data("fixture unreadable\n".utf8)); exit(2) }
    for request in requests {
        guard let op = request["op"] as? String, let keys = required[op] else {
            FileHandle.standardError.write(Data("unknown op in fixture\n".utf8)); exit(2)
        }
        for key in keys where request[key] == nil {
            FileHandle.standardError.write(Data("fixture op \(op) lacks \(key)\n".utf8)); exit(2)
        }
    }
    print("fixture ok")
    exit(0)
}

if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--check-fixture" {
    checkFixture(CommandLine.arguments[2])
}

let input = FileHandle.standardInput.readDataToEndOfFile()
guard let request = try? JSONSerialization.jsonObject(with: input) as? [String: Any],
    let op = request["op"] as? String
else { fail("bad_request") }

let store = EKEventStore()

switch op {
case "authorization":
    if (request["prompt"] as? Bool) == true,
        EKEventStore.authorizationStatus(for: .reminder) == .notDetermined
    {
        let done = DispatchSemaphore(value: 0)
        store.requestFullAccessToReminders { _, _ in done.signal() }
        done.wait()
    }
    emit(["op": "authorization", "status": statusName(EKEventStore.authorizationStatus(for: .reminder))])

case "list_lists":
    guard authorized() else { fail("not_authorized") }
    emit(["op": "list_lists", "count": store.calendars(for: .reminder).count])

case "fetch_changed":
    guard authorized() else { fail("not_authorized") }
    let since = request["since"] as? Int ?? 0
    let offset = max(0, request["offset"] as? Int ?? 0)
    let limit = max(1, min(request["limit"] as? Int ?? 200, 1000))
    let includeCompleted = request["include_completed"] as? Bool ?? true
    let done = DispatchSemaphore(value: 0)
    var found: [EKReminder] = []
    let predicate = includeCompleted
        ? store.predicateForReminders(in: nil)
        : store.predicateForIncompleteReminders(withDueDateStarting: nil, ending: nil, calendars: nil)
    store.fetchReminders(matching: predicate) { result in
        found = result ?? []
        done.signal()
    }
    done.wait()
    let sorted = found
        .map { ($0, Int(($0.lastModifiedDate ?? Date(timeIntervalSince1970: 0)).timeIntervalSince1970)) }
        .filter { $0.1 >= since }
        .sorted { ($0.1, $0.0.calendarItemIdentifier) < ($1.1, $1.0.calendarItemIdentifier) }
    let slice = sorted.dropFirst(offset).prefix(limit).map { wire($0.0) }
    emit(["op": "fetch_changed", "reminders": slice, "has_more": sorted.count > offset + slice.count])

case "create_reminder":
    guard authorized() else { fail("not_authorized") }
    guard let title = request["title"] as? String, !title.isEmpty else { fail("bad_request") }
    let reminder = EKReminder(eventStore: store)
    reminder.title = title
    if let listName = request["list"] as? String {
        guard let calendar = store.calendars(for: .reminder).first(where: { $0.title == listName })
        else { fail("list_not_found") }
        reminder.calendar = calendar
    } else {
        guard let calendar = store.defaultCalendarForNewReminders() else { fail("list_not_found") }
        reminder.calendar = calendar
    }
    if let notes = request["notes"] as? String { reminder.notes = notes }
    if let due = request["due"] as? Int {
        reminder.dueDateComponents = Calendar.current.dateComponents(
            [.year, .month, .day, .hour, .minute], from: Date(timeIntervalSince1970: TimeInterval(due)))
    }
    if let repeatRule = request["repeat"] as? [String: Any], let freqName = repeatRule["freq"] as? String {
        let every = max((repeatRule["every"] as? Int) ?? 1, 1)
        let freq: EKRecurrenceFrequency?
        switch freqName {
        case "day": freq = .daily
        case "week": freq = .weekly
        case "month": freq = .monthly
        case "year": freq = .yearly
        default: freq = nil
        }
        if let freq = freq {
            reminder.addRecurrenceRule(EKRecurrenceRule(recurrenceWith: freq, interval: every, end: nil))
        }
    }
    do { try store.save(reminder, commit: true) } catch { fail("failed", "save failed") }
    emit(["op": "create_reminder", "reminder": wire(reminder)])

case "complete_reminder":
    guard authorized() else { fail("not_authorized") }
    guard let id = request["external_id"] as? String else { fail("bad_request") }
    guard let reminder = store.calendarItem(withIdentifier: id) as? EKReminder else { fail("not_found") }
    if reminder.isCompleted { fail("already_completed") }
    reminder.isCompleted = true
    do { try store.save(reminder, commit: true) } catch { fail("failed", "save failed") }
    emit(["op": "complete_reminder"])

case "update_reminder":
    guard authorized() else { fail("not_authorized") }
    guard let id = request["external_id"] as? String,
          let title = request["title"] as? String, !title.isEmpty else { fail("bad_request") }
    guard let reminder = store.calendarItem(withIdentifier: id) as? EKReminder else { fail("not_found") }
    reminder.title = title
    if let due = request["due"] as? Int {
        reminder.dueDateComponents = Calendar.current.dateComponents(
            [.year, .month, .day, .hour, .minute], from: Date(timeIntervalSince1970: TimeInterval(due)))
    } else {
        reminder.dueDateComponents = nil
    }
    if let extra = request["append_notes"] as? String, !extra.isEmpty {
        reminder.notes = (reminder.notes ?? "").isEmpty ? extra : reminder.notes! + "\n\n" + extra
    }
    do { try store.save(reminder, commit: true) } catch { fail("failed", "save failed") }
    emit(["op": "update_reminder"])

default:
    fail("bad_request")
}
