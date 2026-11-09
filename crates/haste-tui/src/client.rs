//! Blocking daemon client used by the TUI.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
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

#[derive(Clone)]
pub struct Client {
    socket: PathBuf,
    token: String,
}

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
        })
    }

    /// One request, one response.
    ///
    /// # Errors
    /// Connection or framing errors, as text.
    pub fn call(&self, body: RequestBody) -> Result<Response, String> {
        let mut stream =
            UnixStream::connect(&self.socket).map_err(|e| format!("daemon unreachable: {e}"))?;
        write_frame(&mut stream, &Request::new(&self.token, body)).map_err(|e| format!("{e:?}"))?;
        read_frame(&mut stream).map_err(|e| format!("{e:?}"))
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
