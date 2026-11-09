//! `agent.prompt` over the local socket: the one write haste makes to a pane, carrying a proposal
//! result. Blocking (it runs on a worker thread) with hard timeouts.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use haste_core::herdr::agent_name_for;
use serde_json::{json, Value};

use crate::protocol::FeedError;

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TEXT_CHARS: usize = 4000;

/// Prompt a local agent with `text`.
///
/// # Errors
/// [`FeedError::Malformed`] for an invalid agent name or text, [`FeedError::Io`] if the socket is
/// unavailable or herdr answers with an error.
pub fn send_prompt(socket: &Path, agent: &str, text: &str) -> Result<(), FeedError> {
    agent_name_for(agent).map_err(|e| FeedError::Malformed(e.into()))?;
    if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
        return Err(FeedError::Malformed("invalid prompt text".into()));
    }
    let io = |e: std::io::Error| FeedError::Io(e.to_string());
    let mut stream = UnixStream::connect(socket).map_err(io)?;
    stream.set_read_timeout(Some(TIMEOUT)).map_err(io)?;
    stream.set_write_timeout(Some(TIMEOUT)).map_err(io)?;
    let request = json!({ "id": "prompt", "method": "agent.prompt", "params": { "target": agent, "text": text } });
    stream
        .write_all((request.to_string() + "\n").as_bytes())
        .map_err(io)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(io)?;
    let reply: Value =
        serde_json::from_str(line.trim()).map_err(|e| FeedError::Malformed(e.to_string()))?;
    if reply.get("error").is_some() {
        return Err(FeedError::Io("herdr refused the prompt".into()));
    }
    match reply["result"]["type"].as_str() {
        Some("agent_prompted") => Ok(()),
        _ => Err(FeedError::Io("unexpected reply to agent.prompt".into())),
    }
}
