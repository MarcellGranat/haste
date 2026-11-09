//! `agent.prompt` over the socket, against a fake server.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::thread;

use haste_herdr::prompt::send_prompt;
use haste_herdr::FeedError;

fn serve(
    reply: &'static str,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Arc<Mutex<Vec<String>>>,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            log.lock().unwrap().push(line);
            let _ = stream.write_all(format!("{reply}\n").as_bytes());
        }
    });
    (dir, path, seen)
}

#[test]
fn a_prompt_sends_target_and_text_and_no_wait_option() {
    let (_d, path, seen) =
        serve(r#"{"id":"prompt","result":{"type":"agent_prompted","agent":{}}}"#);
    send_prompt(&path, "agent-a", "haste: proposal p1 is rejected").unwrap();
    let request: serde_json::Value = serde_json::from_str(seen.lock().unwrap()[0].trim()).unwrap();
    assert_eq!(request["method"], "agent.prompt");
    assert_eq!(request["params"]["target"], "agent-a");
    assert_eq!(request["params"]["text"], "haste: proposal p1 is rejected");
    assert!(request["params"].get("wait").is_none(), "{request}");
}

#[test]
fn an_error_reply_or_an_unexpected_reply_is_a_failure() {
    let (_d, path, _) = serve(r#"{"id":"prompt","error":{"code":"agent_not_found"}}"#);
    assert!(matches!(
        send_prompt(&path, "agent-a", "hi"),
        Err(FeedError::Io(_))
    ));
    let (_d, path, _) = serve(r#"{"id":"prompt","result":{"type":"ok"}}"#);
    assert!(matches!(
        send_prompt(&path, "agent-a", "hi"),
        Err(FeedError::Io(_))
    ));
}

#[test]
fn invalid_names_and_a_missing_socket_never_reach_a_pane() {
    let (_d, path, seen) = serve(r#"{"id":"prompt","result":{"type":"agent_prompted"}}"#);
    assert!(matches!(
        send_prompt(&path, "Agent A", "hi"),
        Err(FeedError::Malformed(_))
    ));
    assert!(matches!(
        send_prompt(&path, "agent-a", ""),
        Err(FeedError::Malformed(_))
    ));
    assert!(seen.lock().unwrap().is_empty(), "nothing was sent");
    assert!(matches!(
        send_prompt(
            std::path::Path::new("/nonexistent/herdr.sock"),
            "agent-a",
            "hi"
        ),
        Err(FeedError::Io(_))
    ));
}
