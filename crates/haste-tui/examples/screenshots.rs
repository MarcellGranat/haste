//! Draws the TUI with made-up data into SVG files for the README (`docs/screenshots/`).
//!
//!     cargo run -p haste-tui --example screenshots [output-folder]
//!
//! Everything shown is invented (a cast of comic-book characters writing about the usual office
//! trouble); nothing is read from a real mailbox. The real renderer draws every cell, so the
//! pictures show the real layout and colours.

use std::fmt::Write as _;
use std::path::PathBuf;

use haste_core::model::{
    Event, Message, ObjectKind, Reminder, Source, TaskPriority, Thread, Triage,
};
use haste_proto::{ConnectorView, EventView, LastMessage, ReminderView, ThreadView};
use haste_tui::app::{App, Key, View};
use haste_tui::inbox;
use haste_tui::theme;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use unicode_width::UnicodeWidthStr;

const COLS: u16 = 112;
const ROWS: u16 = 34;
/// One character cell, in SVG pixels.
const CW: f64 = 8.6;
const CH: f64 = 18.0;
const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;

fn rgb(c: Color, fallback: Color) -> String {
    let (r, g, b) = match if c == Color::Reset { fallback } else { c } {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0xcd, 0xd6, 0xf4),
    };
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The cells of a buffer as an SVG: a rectangle for each run of one background colour and a text
/// for each glyph, placed on the grid so wide emoji and box lines line up.
fn svg(buf: &Buffer) -> String {
    let (w, h) = (
        f64::from(buf.area.width) * CW,
        f64::from(buf.area.height) * CH + 8.0,
    );
    let mut out = String::new();
    let _ = write!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" \
         viewBox=\"0 0 {w:.0} {h:.0}\" font-family=\"'JetBrains Mono','Menlo','DejaVu Sans Mono',monospace\" \
         font-size=\"14\">\n<rect width=\"100%\" height=\"100%\" rx=\"8\" fill=\"{}\"/>\n",
        rgb(theme::BASE, theme::BASE)
    );
    let mut text = String::new();
    for y in 0..buf.area.height {
        // backgrounds, merged along the row
        let mut x = 0;
        while x < buf.area.width {
            let bg = buf[(x, y)].bg;
            let start = x;
            while x < buf.area.width && buf[(x, y)].bg == bg {
                x += 1;
            }
            if bg != Color::Reset && bg != theme::BASE {
                let _ = writeln!(
                    out,
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"{}\"/>",
                    f64::from(start) * CW,
                    f64::from(y) * CH + 4.0,
                    f64::from(x - start) * CW,
                    CH,
                    rgb(bg, theme::BASE)
                );
            }
        }
        // glyphs
        let mut x = 0;
        while x < buf.area.width {
            let cell = &buf[(x, y)];
            let symbol = cell.symbol();
            let width = u16::try_from(symbol.width()).unwrap_or(1).max(1);
            if symbol.trim().is_empty() {
                x += width;
                continue;
            }
            let bold = cell.modifier.contains(Modifier::BOLD);
            let underline = cell.modifier.contains(Modifier::UNDERLINED);
            let _ = writeln!(
                text,
                "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\"{}{}>{}</text>",
                f64::from(x) * CW,
                f64::from(y) * CH + 17.0,
                rgb(cell.fg, theme::TEXT),
                if bold { " font-weight=\"bold\"" } else { "" },
                if underline {
                    " text-decoration=\"underline\""
                } else {
                    ""
                },
                escape(symbol)
            );
            x += width;
        }
    }
    out.push_str(&text);
    out.push_str("</svg>\n");
    out
}

fn render(app: &App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS)).expect("terminal");
    terminal
        .draw(|f| haste_tui::ui::draw(f, app))
        .expect("draw");
    terminal.backend().buffer().clone()
}

struct Mail<'a> {
    id: &'a str,
    source: Source,
    subject: &'a str,
    who: &'a str,
    ago: i64,
    unread: bool,
    priority: i32,
    snippet: &'a str,
}

fn thread(now: i64, m: &Mail<'_>) -> ThreadView {
    ThreadView {
        thread: Thread {
            id: m.id.into(),
            source: m.source.clone(),
            external_id: m.id.into(),
            subject: m.subject.into(),
            participants: vec![m.who.into()],
            last_activity_at: now - m.ago,
            version: 1,
        },
        triage: {
            let mut t = Triage::new(m.id, ObjectKind::Thread);
            t.priority = m.priority;
            t
        },
        tags: vec![],
        last: Some(LastMessage {
            sender: Some(m.who.into()),
            snippet: m.snippet.into(),
            has_attachment: false,
        }),
        unread: m.unread,
        replied: false,
        sender_known: true,
        sender: Some(m.who.into()),
    }
}

fn threads(now: i64) -> Vec<ThreadView> {
    let gmail = Source::Gmail;
    let list = [
        Mail {
            id: "fury",
            source: gmail.clone(),
            subject: "URGENT: assemble the team. Again.",
            who: "Nick Fury",
            ago: 8 * 60,
            unread: true,
            priority: 2,
            snippet: "We have a situation. It is big. It is green. Also purple. Call me back.",
        },
        Mail {
            id: "tony",
            source: gmail.clone(),
            subject: "Re: arc reactor v4, please do not touch the red button",
            who: "Tony Stark",
            ago: 35 * 60,
            unread: true,
            priority: 0,
            snippet: "Define 'touch'. Asking for a friend who is also me.",
        },
        Mail {
            id: "bruce",
            source: gmail.clone(),
            subject: "Please stop sending me deadlines in ALL CAPS",
            who: "Bruce Banner",
            ago: 3 * HOUR,
            unread: true,
            priority: 0,
            snippet: "I am trying to stay calm. You are not helping. This is my calm voice.",
        },
        Mail {
            id: "wanda",
            source: gmail.clone(),
            subject: "Re: reality is not an Excel sheet",
            who: "Wanda Maximoff",
            ago: 5 * HOUR,
            unread: false,
            priority: 0,
            snippet: "I only changed one cell. How was I to know it was the whole budget?",
        },
        Mail {
            id: "loki",
            source: gmail.clone(),
            subject: "Your invitation to a Totally Legit Event",
            who: "Loki Laufeyson",
            ago: 8 * HOUR,
            unread: false,
            priority: 0,
            snippet: "No tricks this time. Bring your own cape. Do not look under the table.",
        },
        Mail {
            id: "steve",
            source: gmail,
            subject: "Language.",
            who: "Steve Rogers",
            ago: DAY,
            unread: false,
            priority: 0,
            snippet: "I understood that reference. Please stop sending me memes before 9 am.",
        },
        Mail {
            id: "natasha",
            source: Source::Whatsapp,
            subject: "Natasha",
            who: "Natasha Romanoff",
            ago: 20 * 60,
            unread: true,
            priority: 0,
            snippet: "Dinner? I am buying. (I am not.)",
        },
        Mail {
            id: "thor",
            source: Source::Messenger,
            subject: "Thor 🔨",
            who: "Thor Odinson",
            ago: 90 * 60,
            unread: true,
            priority: 0,
            snippet: "Mjolnir lost AGAIN. Have you seen it? It was shiny.",
        },
        Mail {
            id: "peter",
            source: Source::Imessage,
            subject: "Peter 🕷️",
            who: "Peter Parker",
            ago: 2 * HOUR,
            unread: false,
            priority: 0,
            snippet: "Mr. Stark, I think I broke the thing again",
        },
        Mail {
            id: "groot",
            source: Source::Whatsapp,
            subject: "Groot",
            who: "Groot",
            ago: 6 * HOUR,
            unread: false,
            priority: 0,
            snippet: "I am Groot. (He means: the tree needs watering.)",
        },
        Mail {
            id: "strange",
            source: Source::GoogleChat,
            subject: "Stephen Strange",
            who: "Stephen Strange",
            ago: 2 * DAY,
            unread: false,
            priority: 0,
            snippet: "I looked at 14,000,605 futures. In one of them you reply on time.",
        },
    ];
    list.iter().map(|m| thread(now, m)).collect()
}

fn todo(now: i64, id: &str, title: &str, due: Option<i64>, priority: TaskPriority) -> ReminderView {
    ReminderView {
        reminder: Reminder {
            id: id.into(),
            external_id: id.into(),
            title: title.into(),
            notes_snippet: String::new(),
            due_at: due,
            completed: false,
            list_name: "Avengers".into(),
            remote_modified_at: now,
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

fn todos(now: i64) -> Vec<ReminderView> {
    vec![
        todo(
            now,
            "t1",
            "Save the world (before Friday)",
            Some(now - 2 * HOUR),
            TaskPriority::Urgent,
        ),
        todo(
            now,
            "t2",
            "Return Tony's suit (not mine, I swear)",
            Some(now - DAY),
            TaskPriority::High,
        ),
        todo(
            now,
            "t3",
            "Explain to HR why there is a hole in the roof",
            Some(now + 2 * HOUR),
            TaskPriority::Middle,
        ),
        todo(
            now,
            "t4",
            "Buy more shawarma (a lot more)",
            Some(now + 4 * HOUR),
            TaskPriority::Middle,
        ),
        todo(
            now,
            "t5",
            "Ask Thor to stop calling lightning 'Wi-Fi'",
            Some(now + DAY),
            TaskPriority::Low,
        ),
        todo(
            now,
            "t6",
            "Change the password from 'avengers123'",
            None,
            TaskPriority::High,
        ),
        todo(
            now,
            "t7",
            "Find Cap's shield (again)",
            None,
            TaskPriority::Middle,
        ),
    ]
}

fn event(
    now: i64,
    id: &str,
    calendar: &str,
    name: &str,
    title: &str,
    start: i64,
    minutes: i64,
) -> EventView {
    let _ = now;
    EventView {
        event: Event {
            id: id.into(),
            external_id: id.into(),
            calendar: calendar.into(),
            title: title.into(),
            start_at: start,
            end_at: start + minutes * 60,
            all_day: false,
            status: "confirmed".into(),
            attendees: vec![],
            location: String::new(),
            description_snippet: String::new(),
            updated_at: 0,
            missing_since: None,
            version: 1,
        },
        tags: vec![],
        shifted: false,
        calendar_name: name.into(),
    }
}

fn events(now: i64, off: i64) -> Vec<EventView> {
    let monday = inbox::week_start(now, off, 0);
    let at = |day: i64, hour: i64, min: i64| monday + day * DAY + hour * HOUR + min * 60;
    vec![
        event(
            now,
            "e1",
            "hq",
            "Avengers HQ",
            "Stand-up (15 min, it was 3 h)",
            at(0, 9, 30),
            45,
        ),
        event(
            now,
            "e2",
            "hq",
            "Avengers HQ",
            "Retro: who broke New York?",
            at(0, 14, 0),
            90,
        ),
        event(
            now,
            "e3",
            "shield",
            "S.H.I.E.L.D.",
            "1:1 with Fury (bring snacks)",
            at(1, 11, 0),
            60,
        ),
        event(
            now,
            "e4",
            "hq",
            "Avengers HQ",
            "Sprint planning: infinity edition",
            at(1, 14, 0),
            120,
        ),
        event(now, "e5", "life", "Life", "Shawarma", at(2, 13, 0), 45),
        event(
            now,
            "e6",
            "shield",
            "S.H.I.E.L.D.",
            "Mandatory fun (attendance recorded)",
            at(2, 16, 0),
            60,
        ),
        event(
            now,
            "e7",
            "hq",
            "Avengers HQ",
            "Demo: it works on my planet",
            at(3, 10, 0),
            60,
        ),
        event(
            now,
            "e8",
            "life",
            "Life",
            "Therapy with Bruce",
            at(3, 15, 30),
            60,
        ),
        event(
            now,
            "e9",
            "hq",
            "Avengers HQ",
            "Save the world",
            at(4, 9, 0),
            180,
        ),
        event(
            now,
            "e10",
            "life",
            "Life",
            "Friday beers (Thor pays)",
            at(4, 17, 0),
            120,
        ),
    ]
}

fn connectors(now: i64) -> Vec<ConnectorView> {
    let view = |source: &str, engaged: bool, ago: Option<i64>| ConnectorView {
        source: source.into(),
        engaged,
        registered: true,
        last_synced: ago.map(|a| now - a),
    };
    vec![
        view("gmail", false, Some(90)),
        view("icloud", false, Some(210)),
        view("whatsapp", false, Some(40)),
        view("messenger", false, Some(40)),
        view("imessage", true, Some(3 * HOUR)),
        view("googlechat", false, Some(40)),
        view("gcal", false, Some(130)),
        view("reminders", false, Some(60)),
    ]
}

fn base(now: i64) -> App {
    let mut app = App::new();
    app.tz_offset = 3_600;
    app.set_threads(threads(now));
    app.set_reminder_list(todos(now));
    app.set_events(events(now, 3_600));
    app.set_connectors(connectors(now));
    app
}

fn message(id: &str, thread: &str, who: &str, body: &str, at: i64, own: bool) -> Message {
    Message {
        id: id.into(),
        thread_id: thread.into(),
        source: Source::Gmail,
        external_id: id.into(),
        sender_person_id: None,
        sender_identifier: Some(who.into()),
        body_text: body.into(),
        sent_at: at,
        is_own: own,
    }
}

fn save(dir: &std::path::Path, name: &str, buf: &Buffer) {
    std::fs::write(dir.join(name), svg(buf)).expect("write svg");
    println!("wrote {name}");
}

fn main() {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("docs/screenshots"), PathBuf::from);
    std::fs::create_dir_all(&dir).expect("folder");
    let now = haste_core::time::now_secs();

    // 1: Today
    let app = base(now);
    save(&dir, "today.svg", &render(&app));

    // 2: Channels, with the other connectors
    let mut app = base(now);
    app.handle_key(Key::Char('2'));
    app.handle_key(Key::Tab);
    save(&dir, "channels.svg", &render(&app));

    // 3: Calendar
    let mut app = base(now);
    app.view = View::Calendar;
    app.handle_key(Key::Char('3'));
    save(&dir, "calendar.svg", &render(&app));

    // 4: a thread with the reply box open under it
    let mut app = base(now);
    let view = threads(now).into_iter().next().expect("fury");
    app.show_thread(
        view,
        vec![
            message(
                "m1",
                "fury",
                "Nick Fury",
                "Stark. Rogers. Banner. Everyone. Conference room. Now.",
                now - 40 * 60,
                false,
            ),
            message("m2", "fury", "you", "Define 'now'.", now - 31 * 60, true),
            message(
                "m3",
                "fury",
                "Nick Fury",
                "We have a situation. It is big. It is green. Also purple.",
                now - 8 * 60,
                false,
            ),
        ],
        Some("you".into()),
    );
    app.handle_key(Key::Char('r'));
    app.open_reply_form("fury");
    for c in "On my way. Please do not let Thor near the coffee machine.".chars() {
        app.handle_key(Key::Char(c));
    }
    save(&dir, "reply.svg", &render(&app));
}
