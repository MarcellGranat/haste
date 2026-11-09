//! A minimal SMTP submission client for a human send. It is deliberately phase-aware: the
//! at-most-once rule needs to know whether the server refused *before* it accepted the message
//! body (nothing was delivered) or whether the body was handed over and the outcome is unknown.
//!
//! Each command is sent exactly once; there is no retry anywhere in this module. Errors carry
//! only fixed labels: never an address, a subject, a body, a password or server text.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use haste_core::send::SEND_TIMEOUT_SECS;

use crate::config::IcloudConfig;

/// Longest reply line accepted from the server.
const MAX_LINE: usize = 4096;
/// What we call ourselves in `EHLO`.
const EHLO_NAME: &str = "haste.local";

/// Why a submission did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpFailure {
    /// No connection, resolution failure or TLS handshake failure: nothing was sent.
    Connect(&'static str),
    /// The server rejected the login: nothing was sent, and retrying cannot help.
    AuthRejected,
    /// Refused before the message body was accepted for transfer: nothing was delivered.
    Refused(&'static str),
    /// The body was (or may have been) handed over and the outcome is unknown.
    Ambiguous(&'static str),
}

/// A connection to the submission server, upgradable with STARTTLS.
pub trait SmtpStream: Read + Write + Send {
    /// Whether STARTTLS should be negotiated on this connection (a plain production socket
    /// does; an already-encrypted one and the loopback test socket do not).
    fn can_upgrade(&self) -> bool;
    /// Wrap the connection in TLS after the server's `220` to `STARTTLS`.
    ///
    /// # Errors
    /// [`SmtpFailure::Connect`] when the handshake or certificate check fails.
    fn upgrade(self: Box<Self>, host: &str) -> Result<Box<dyn SmtpStream>, SmtpFailure>;
}

/// Opens connections to the submission server. Production uses [`TlsSmtp`].
pub trait SmtpTransport: Send + Sync {
    /// # Errors
    /// [`SmtpFailure::Connect`] when no connection can be made.
    fn connect(&self, config: &IcloudConfig) -> Result<Box<dyn SmtpStream>, SmtpFailure>;
}

fn tcp(config: &IcloudConfig, timeout: Duration) -> Result<TcpStream, SmtpFailure> {
    let addr = (config.smtp_host.as_str(), config.smtp_port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .ok_or(SmtpFailure::Connect("cannot resolve host"))?;
    let stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|_| SmtpFailure::Connect("cannot connect"))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    Ok(stream)
}

struct Plain(TcpStream);
struct Secure(native_tls::TlsStream<TcpStream>);

macro_rules! io_passthrough {
    ($t:ty, $field:tt) => {
        impl Read for $t {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.$field.read(buf)
            }
        }
        impl Write for $t {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.$field.write(buf)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.$field.flush()
            }
        }
    };
}
io_passthrough!(Plain, 0);
io_passthrough!(Secure, 0);

impl SmtpStream for Plain {
    fn can_upgrade(&self) -> bool {
        true
    }
    fn upgrade(self: Box<Self>, host: &str) -> Result<Box<dyn SmtpStream>, SmtpFailure> {
        let connector = native_tls::TlsConnector::builder()
            .build()
            .map_err(|_| SmtpFailure::Connect("tls setup failed"))?;
        let tls = connector
            .connect(host, self.0)
            .map_err(|_| SmtpFailure::Connect("tls handshake failed"))?;
        Ok(Box::new(Secure(tls)))
    }
}

impl SmtpStream for Secure {
    fn can_upgrade(&self) -> bool {
        false
    }
    fn upgrade(self: Box<Self>, _host: &str) -> Result<Box<dyn SmtpStream>, SmtpFailure> {
        Ok(self)
    }
}

/// Submission on the configured port with STARTTLS and certificate verification on.
#[derive(Default)]
pub struct TlsSmtp;

impl SmtpTransport for TlsSmtp {
    fn connect(&self, config: &IcloudConfig) -> Result<Box<dyn SmtpStream>, SmtpFailure> {
        Ok(Box::new(Plain(tcp(
            config,
            Duration::from_secs(SEND_TIMEOUT_SECS),
        )?)))
    }
}

/// Loopback plaintext for tests against a fake server (no STARTTLS). Never used in production.
pub struct PlainSmtp {
    pub timeout: Duration,
}

impl Default for PlainSmtp {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
        }
    }
}

struct Loopback(TcpStream);
io_passthrough!(Loopback, 0);

impl SmtpStream for Loopback {
    fn can_upgrade(&self) -> bool {
        false
    }
    fn upgrade(self: Box<Self>, _host: &str) -> Result<Box<dyn SmtpStream>, SmtpFailure> {
        Ok(self)
    }
}

impl SmtpTransport for PlainSmtp {
    fn connect(&self, config: &IcloudConfig) -> Result<Box<dyn SmtpStream>, SmtpFailure> {
        Ok(Box::new(Loopback(tcp(config, self.timeout)?)))
    }
}

/// Line-oriented wrapper around a stream with a small read buffer.
struct Wire {
    conn: Box<dyn SmtpStream>,
    buf: Vec<u8>,
}

impl Wire {
    fn send(&mut self, line: &str) -> io::Result<()> {
        self.conn.write_all(line.as_bytes())?;
        self.conn.write_all(b"\r\n")?;
        self.conn.flush()
    }

    fn line(&mut self) -> io::Result<String> {
        loop {
            if let Some(pos) = self.buf.windows(2).position(|w| w == b"\r\n") {
                let line: Vec<u8> = self.buf.drain(..pos + 2).collect();
                return Ok(String::from_utf8_lossy(&line[..pos]).into_owned());
            }
            if self.buf.len() > MAX_LINE {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
            }
            let mut chunk = [0_u8; 1024];
            let n = self.conn.read(&mut chunk)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed"));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// One (possibly multi-line) reply: the code and its text lines.
    fn reply(&mut self) -> io::Result<(u16, Vec<String>)> {
        let mut lines = Vec::new();
        loop {
            let line = self.line()?;
            let code: u16 = line
                .get(..3)
                .and_then(|c| c.parse().ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad reply"))?;
            let more = line.as_bytes().get(3) == Some(&b'-');
            lines.push(line.get(4..).unwrap_or("").to_owned());
            if !more {
                return Ok((code, lines));
            }
        }
    }

    /// Send a command and read its reply.
    fn command(&mut self, line: &str) -> io::Result<(u16, Vec<String>)> {
        self.send(line)?;
        self.reply()
    }
}

fn advertises(caps: &[String], word: &str) -> bool {
    caps.iter().any(|l| {
        l.split_whitespace()
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case(word))
    })
}

fn supports_auth(caps: &[String], mechanism: &str) -> bool {
    caps.iter().any(|l| {
        let mut words = l.split_whitespace();
        words.next().is_some_and(|w| w.eq_ignore_ascii_case("AUTH"))
            && words.any(|m| m.eq_ignore_ascii_case(mechanism))
    })
}

/// Dot-stuff a message and terminate it: lines starting with `.` get a second dot, and the data
/// ends with `CRLF . CRLF`.
fn body_bytes(message: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 8);
    let mut at_line_start = true;
    for b in message.bytes() {
        if at_line_start && b == b'.' {
            out.push(b'.');
        }
        out.push(b);
        at_line_start = b == b'\n';
    }
    if !message.ends_with("\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

/// Submit one message. `from` and `to` are bare, validated addresses; `message` is the complete
/// RFC 5322 text with CRLF line ends. One attempt: nothing here repeats a command.
///
/// # Errors
/// [`SmtpFailure`]: `Connect`, `AuthRejected` and `Refused` mean nothing was delivered;
/// `Ambiguous` means the body was handed over and the outcome is unknown.
pub fn submit(
    transport: &dyn SmtpTransport,
    config: &IcloudConfig,
    password: &str,
    from: &str,
    to: &str,
    message: &str,
) -> Result<(), SmtpFailure> {
    let conn = transport.connect(config)?;
    let mut wire = Wire {
        conn,
        buf: Vec::new(),
    };
    match wire.reply() {
        Ok((220, _)) => {}
        _ => return Err(SmtpFailure::Connect("greeting")),
    }
    let ehlo = |wire: &mut Wire| match wire.command(&format!("EHLO {EHLO_NAME}")) {
        Ok((250, caps)) => Ok(caps),
        _ => Err(SmtpFailure::Refused("ehlo")),
    };
    let mut caps = ehlo(&mut wire)?;

    if wire.conn.can_upgrade() {
        if !advertises(&caps, "STARTTLS") {
            return Err(SmtpFailure::Refused("starttls unavailable"));
        }
        match wire.command("STARTTLS") {
            Ok((220, _)) => {}
            _ => return Err(SmtpFailure::Refused("starttls")),
        }
        wire.conn = wire.conn.upgrade(&config.smtp_host)?;
        wire.buf.clear();
        caps = ehlo(&mut wire)?;
    }

    authenticate(&mut wire, &caps, &config.address, password)?;

    match wire.command(&format!("MAIL FROM:<{from}>")) {
        Ok((250, _)) => {}
        _ => return Err(SmtpFailure::Refused("mail from")),
    }
    match wire.command(&format!("RCPT TO:<{to}>")) {
        Ok((250 | 251, _)) => {}
        _ => return Err(SmtpFailure::Refused("rcpt to")),
    }
    match wire.command("DATA") {
        Ok((354, _)) => {}
        _ => return Err(SmtpFailure::Refused("data")),
    }

    // From here the message may reach the recipient: every failure is ambiguous.
    if wire.conn.write_all(&body_bytes(message)).is_err() || wire.conn.flush().is_err() {
        return Err(SmtpFailure::Ambiguous("body write"));
    }
    match wire.reply() {
        Ok((250, _)) => {}
        Ok(_) => return Err(SmtpFailure::Ambiguous("final reply")),
        Err(_) => return Err(SmtpFailure::Ambiguous("no final reply")),
    }
    let _ = wire.send("QUIT"); // best effort: the message is already accepted
    Ok(())
}

fn authenticate(
    wire: &mut Wire,
    caps: &[String],
    user: &str,
    password: &str,
) -> Result<(), SmtpFailure> {
    let rejected = |code: u16| {
        if (500..600).contains(&code) {
            SmtpFailure::AuthRejected
        } else {
            SmtpFailure::Refused("auth")
        }
    };
    if supports_auth(caps, "PLAIN") {
        let token = STANDARD.encode(format!("\0{user}\0{password}"));
        return match wire.command(&format!("AUTH PLAIN {token}")) {
            Ok((235, _)) => Ok(()),
            Ok((code, _)) => Err(rejected(code)),
            Err(_) => Err(SmtpFailure::Refused("auth")),
        };
    }
    if supports_auth(caps, "LOGIN") {
        let step = |wire: &mut Wire, line: &str| {
            wire.command(line).map_err(|_| SmtpFailure::Refused("auth"))
        };
        let (code, _) = step(wire, "AUTH LOGIN")?;
        if code != 334 {
            return Err(rejected(code));
        }
        let (code, _) = step(wire, &STANDARD.encode(user))?;
        if code != 334 {
            return Err(rejected(code));
        }
        let (code, _) = step(wire, &STANDARD.encode(password))?;
        return if code == 235 {
            Ok(())
        } else {
            Err(rejected(code))
        };
    }
    Err(SmtpFailure::Refused("no auth mechanism"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_stuffing_and_termination() {
        assert_eq!(
            body_bytes("a\r\n.b\r\n..c\r\nend"),
            b"a\r\n..b\r\n...c\r\nend\r\n.\r\n"
        );
        assert_eq!(body_bytes("x\r\n"), b"x\r\n.\r\n");
        assert_eq!(body_bytes(".only"), b"..only\r\n.\r\n");
    }

    #[test]
    fn capabilities_are_matched_by_word_not_substring() {
        let caps = vec![
            "smtp.mail.me.com".to_owned(),
            "AUTH LOGIN PLAIN".to_owned(),
            "STARTTLS".to_owned(),
        ];
        assert!(advertises(&caps, "starttls") && !advertises(&caps, "PIPELINING"));
        assert!(supports_auth(&caps, "plain") && supports_auth(&caps, "LOGIN"));
        assert!(!supports_auth(&caps, "XOAUTH2"));
        // a hostname that merely contains the word is not a capability
        assert!(!advertises(&["starttls.example".to_owned()], "STARTTLS"));
    }
}
