//! Blocking daemon client used by the TUI.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

use haste_proto::{read_frame, write_frame, Event, Request, RequestBody, Response, Topic};

const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const DEFAULT_BASE: &str = "Library/Application Support/haste";

#[must_use]
pub fn base_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HASTE_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(DEFAULT_BASE)
}

/// A client with one connection kept open between requests: opening one costs a round trip (and,
/// through an ssh forward to another machine, a whole new channel), so a screen that asks the
/// daemon a dozen things would otherwise wait a dozen times. A clone gets a connection of its
/// own, so a slow request on one thread never holds up the others.
pub struct Client {
    socket: PathBuf,
    token: String,
    connection: std::sync::Mutex<Option<UnixStream>>,
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            socket: self.socket.clone(),
            token: self.token.clone(),
            connection: std::sync::Mutex::new(None),
        }
    }
}

/// Where an exchange failed: before the daemon can have acted on the request, or after.
enum Failed {
    Writing(String),
    Reading(String),
}

fn exchange(stream: &mut UnixStream, request: &Request) -> Result<Response, Failed> {
    write_frame(stream, request).map_err(|e| Failed::Writing(format!("{e:?}")))?;
    read_frame(stream).map_err(|e| Failed::Reading(format!("{e:?}")))
}

/// Requests that only read, so asking again after a lost answer is harmless. Anything else (a
/// send, an archive, a write) is never asked twice.
fn only_reads(body: &RequestBody) -> bool {
    matches!(
        body,
        RequestBody::ThreadList { .. }
            | RequestBody::ThreadShow { .. }
            | RequestBody::ThreadSearch { .. }
            | RequestBody::ProposalList { .. }
            | RequestBody::ProposalStatus { .. }
            | RequestBody::ScheduledList
            | RequestBody::AgentList
            | RequestBody::ConnectorStatus
            | RequestBody::RemindersStatus
            | RequestBody::RemindersList { .. }
            | RequestBody::EventsList { .. }
            | RequestBody::SenderRules
            | RequestBody::TagList
            | RequestBody::FilterList
    )
}

/// Whether the last connection attempt failed, so an outage is logged once.
static DAEMON_DOWN: AtomicBool = AtomicBool::new(false);

impl Client {
    /// Read the human token and remember the socket location.
    ///
    /// # Errors
    /// Fails when the human token file is missing (is the daemon bootstrapped?).
    pub fn from_base(base: &Path) -> std::io::Result<Self> {
        let token = std::fs::read_to_string(base.join("human.token"))?
            .trim()
            .to_owned();
        Ok(Self {
            socket: base.join("run").join("hasted.sock"),
            token,
            connection: std::sync::Mutex::new(None),
        })
    }

    /// One request, one response, over the connection kept from the last call. A connection that
    /// has gone stale is replaced: at once when the request was never written, and only for a
    /// request that just reads when the answer was lost (a write is never sent twice).
    ///
    /// # Errors
    /// Connection or framing errors, as text.
    pub fn call(&self, body: RequestBody) -> Result<Response, String> {
        let reads_only = only_reads(&body);
        let request = Request::new(&self.token, body);
        let mut kept = self
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(stream) = kept.as_mut() {
            match exchange(stream, &request) {
                Ok(response) => return Ok(response),
                Err(Failed::Writing(_)) => *kept = None,
                Err(Failed::Reading(e)) => {
                    *kept = None;
                    if !reads_only {
                        return Err(e);
                    }
                }
            }
        }
        let mut stream = self.connect()?;
        let answer = exchange(&mut stream, &request);
        match answer {
            Ok(response) => {
                *kept = Some(stream);
                Ok(response)
            }
            Err(Failed::Writing(e) | Failed::Reading(e)) => Err(e),
        }
    }

    fn connect(&self) -> Result<UnixStream, String> {
        match UnixStream::connect(&self.socket) {
            Ok(s) => {
                if DAEMON_DOWN.swap(false, Ordering::Relaxed) {
                    haste_core::log::info("tui", "daemon_reachable_again", &[]);
                }
                Ok(s)
            }
            Err(e) => {
                // once per outage, not once per refresh
                if !DAEMON_DOWN.swap(true, Ordering::Relaxed) {
                    haste_core::log::error(
                        "tui",
                        "daemon_unreachable",
                        &[haste_core::log::id("kind", &format!("{:?}", e.kind()))],
                    );
                }
                Err(format!("daemon unreachable: {e}"))
            }
        }
    }

    /// Subscribe in a background thread; events arrive on the returned channel.
    /// The thread reconnects if the daemon restarts.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = channel();
        let client = self.clone();
        thread::spawn(move || loop {
            if let Ok(mut stream) = UnixStream::connect(&client.socket) {
                let request = Request::new(
                    &client.token,
                    RequestBody::Subscribe {
                        topics: vec![Topic::Threads, Topic::Proposals, Topic::Notices],
                    },
                );
                if write_frame(&mut stream, &request).is_ok() {
                    while let Ok(response) = read_frame::<_, Response>(&mut stream) {
                        if let Response::Event { event } = response {
                            if tx.send(event).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            thread::sleep(RECONNECT_DELAY);
        });
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    /// A daemon that answers `Done` to every request on a connection and counts connections and
    /// requests; `hang_up_after` closes a connection after that many requests (0 = never), and
    /// `without_answering` makes it take a connection's second request and hang up unanswered.
    fn fake_daemon(
        dir: &std::path::Path,
        hang_up_after: usize,
        without_answering: bool,
    ) -> (Client, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let sock = dir.join("run");
        std::fs::create_dir_all(&sock).unwrap();
        let listener = UnixListener::bind(sock.join("hasted.sock")).unwrap();
        let (connections, requests) =
            (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (c, r) = (Arc::clone(&connections), Arc::clone(&requests));
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                c.fetch_add(1, Ordering::SeqCst);
                let r = Arc::clone(&r);
                thread::spawn(move || {
                    let mut served = 0;
                    while read_frame::<_, Request>(&mut stream).is_ok() {
                        r.fetch_add(1, Ordering::SeqCst);
                        served += 1;
                        if without_answering && served == 2 {
                            return; // hang up with the answer lost
                        }
                        if write_frame(&mut stream, &Response::Done).is_err() {
                            return;
                        }
                        if hang_up_after != 0 && served >= hang_up_after {
                            return;
                        }
                    }
                });
            }
        });
        std::fs::write(dir.join("human.token"), "t\\n").unwrap();
        (Client::from_base(dir).unwrap(), connections, requests)
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("haste-client-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn requests_share_one_connection_and_a_clone_has_its_own() {
        let dir = temp("share");
        let (client, connections, requests) = fake_daemon(&dir, 0, false);
        for _ in 0..5 {
            assert_eq!(client.call(RequestBody::AgentList), Ok(Response::Done));
        }
        assert_eq!(
            connections.load(Ordering::SeqCst),
            1,
            "one connection for five requests"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 5);
        let other = client.clone();
        assert_eq!(other.call(RequestBody::AgentList), Ok(Response::Done));
        assert_eq!(
            connections.load(Ordering::SeqCst),
            2,
            "a clone connects for itself"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_connection_the_daemon_closed_is_replaced_before_the_request_goes_out() {
        let dir = temp("stale");
        let (client, connections, requests) = fake_daemon(&dir, 1, false);
        // every connection is closed after its first answer: the next call finds it dead
        for _ in 0..3 {
            assert_eq!(
                client.call(RequestBody::EventsList { from: 0 }),
                Ok(Response::Done)
            );
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            3,
            "each request was served once"
        );
        assert!(connections.load(Ordering::SeqCst) >= 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lost_answer_asks_a_read_again_but_never_a_write() {
        // a read whose answer is lost on a kept connection is asked again on a new one
        let dir = temp("lost-read");
        let (client, _connections, requests) = fake_daemon(&dir, 0, true);
        assert_eq!(client.call(RequestBody::AgentList), Ok(Response::Done));
        assert_eq!(
            client.call(RequestBody::AgentList),
            Ok(Response::Done),
            "asked again"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        // a write whose answer is lost is not: the daemon may have acted on it
        let dir2 = temp("lost-write");
        let (client2, _c, requests2) = fake_daemon(&dir2, 0, true);
        assert_eq!(client2.call(RequestBody::AgentList), Ok(Response::Done));
        let write = RequestBody::ConnectorEngage {
            source: "gmail".into(),
        };
        assert!(client2.call(write).is_err());
        assert_eq!(requests2.load(Ordering::SeqCst), 2, "a write is asked once");
        // and the next call opens a fresh connection
        assert_eq!(client2.call(RequestBody::AgentList), Ok(Response::Done));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
