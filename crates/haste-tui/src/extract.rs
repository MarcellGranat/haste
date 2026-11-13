//! Plain text out of the two kinds of attachment the reader previews: PDF and Word. It runs the
//! system tools (`pdftotext` from poppler, macOS `textutil`), never anything from the file, and
//! gives up after a time limit. Nothing is rendered and nothing is opened in an application.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a tool may run, and how much text is kept.
pub const EXTRACT_TIMEOUT: Duration = Duration::from_secs(20);
pub const EXTRACT_MAX_BYTES: u64 = 400 * 1024;

/// The previewable kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Word,
}

/// PDF or Word (`.doc`, `.docx`) by extension, else by media type; `None` for anything else.
#[must_use]
pub fn kind_of(name: &str, mime: &str) -> Option<Kind> {
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
    let mime = mime.to_lowercase();
    match ext.as_str() {
        "pdf" => return Some(Kind::Pdf),
        "doc" | "docx" => return Some(Kind::Word),
        _ => {}
    }
    match mime.as_str() {
        "application/pdf" => Some(Kind::Pdf),
        "application/msword"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(Kind::Word)
        }
        _ => None,
    }
}

/// The text of a file, or a short reason it could not be read.
///
/// # Errors
/// The tool is missing, fails, or runs too long.
pub fn extract_text(path: &Path, kind: Kind) -> Result<String, String> {
    let mut command = match kind {
        Kind::Pdf => {
            let mut c = Command::new("pdftotext");
            c.args(["-layout", "-q"]).arg(path).arg("-");
            c
        }
        Kind::Word => {
            let mut c = Command::new("textutil");
            c.args(["-convert", "txt", "-stdout"]).arg(path);
            c
        }
    };
    let tool = if kind == Kind::Pdf {
        "pdftotext"
    } else {
        "textutil"
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            if kind == Kind::Pdf {
                "pdftotext is not installed (brew install poppler)".to_owned()
            } else {
                "textutil is not available".to_owned()
            }
        })?;
    let mut stdout = child.stdout.take().ok_or("no output")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(EXTRACT_MAX_BYTES).read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() > EXTRACT_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{tool} took too long"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return Err(format!("{tool} failed")),
        }
    };
    let bytes = reader.join().unwrap_or_default();
    if !status.success() && bytes.is_empty() {
        return Err(format!("{tool} could not read this file"));
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.trim().is_empty() {
        return Err("no text in this file (a scan or an image?)".to_owned());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pdf_and_word_are_previewed() {
        assert_eq!(kind_of("a.PDF", ""), Some(Kind::Pdf));
        assert_eq!(kind_of("a.docx", ""), Some(Kind::Word));
        assert_eq!(kind_of("a.doc", ""), Some(Kind::Word));
        assert_eq!(kind_of("noext", "application/pdf"), Some(Kind::Pdf));
        assert_eq!(kind_of("a.png", "image/png"), None);
        assert_eq!(kind_of("a.xlsx", ""), None);
        assert_eq!(kind_of("a.txt", "text/plain"), None);
    }

    #[test]
    fn a_missing_file_is_a_short_reason_not_a_hang() {
        let r = extract_text(Path::new("/no/such/file.pdf"), Kind::Pdf);
        assert!(r.is_err());
    }
}
