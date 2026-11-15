//! What the reader previews from an attachment: text out of PDF, Word and PowerPoint, and
//! pictures. It runs the system tools (`pdftotext` from poppler, macOS `textutil`, `unzip`,
//! macOS `sips`), never anything from the file, and gives up after a time limit. Nothing is
//! opened in an application.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long a tool may run, and how much text is kept.
pub const EXTRACT_TIMEOUT: Duration = Duration::from_secs(20);
pub const EXTRACT_MAX_BYTES: u64 = 400 * 1024;
/// Most slides read from one presentation.
const MAX_SLIDES: usize = 300;
/// The longest side, in pixels, a picture is shrunk to before it is drawn.
const IMAGE_MAX_SIDE: &str = "320";
/// Largest converted picture read back (a 320 px bitmap is far below this).
const IMAGE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The previewable kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Word,
    Slides,
    Image,
}

/// The kind by extension, else by media type; `None` for anything else.
#[must_use]
pub fn kind_of(name: &str, mime: &str) -> Option<Kind> {
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
    let mime = mime.to_lowercase();
    match ext.as_str() {
        "pdf" => return Some(Kind::Pdf),
        "doc" | "docx" | "rtf" | "odt" => return Some(Kind::Word),
        "ppt" | "pptx" => return Some(Kind::Slides),
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tif" | "tiff" | "heic" | "heif" | "webp" => {
            return Some(Kind::Image);
        }
        _ => {}
    }
    match mime.as_str() {
        "application/pdf" => Some(Kind::Pdf),
        "application/msword"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(Kind::Word)
        }
        "application/vnd.ms-powerpoint"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some(Kind::Slides)
        }
        m if m.starts_with("image/") && !m.contains("svg") => Some(Kind::Image),
        _ => None,
    }
}

/// Run a tool, keep at most `max` bytes of its output, and kill it after the time limit.
fn run_tool(mut command: Command, tool: &str, max: u64) -> Result<(ExitStatus, Vec<u8>), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| format!("{tool} is not available"))?;
    let mut stdout = child.stdout.take().ok_or("no output")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(max).read_to_end(&mut buf);
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
    Ok((status, reader.join().unwrap_or_default()))
}

/// The text of `<a:t>` runs in one slide's XML, a line per paragraph.
#[must_use]
pub fn slide_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        let Some(close) = rest.find('>') else { break };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        if tag == "/a:p" || tag == "a:br/" || tag == "a:br" {
            out.push('\n');
        } else if (tag == "a:t" || tag.starts_with("a:t ")) && !tag.ends_with('/') {
            let end = rest.find("</a:t>").unwrap_or(rest.len());
            out.push_str(&unescape(&rest[..end]));
            rest = &rest[end..];
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn slides_text(path: &Path) -> Result<String, String> {
    let mut list = Command::new("unzip");
    list.arg("-Z1").arg(path);
    let (status, names) = run_tool(list, "unzip", EXTRACT_MAX_BYTES)?;
    if !status.success() {
        return Err("only .pptx can be read here (an old .ppt: d downloads it)".to_owned());
    }
    let names = String::from_utf8_lossy(&names).into_owned();
    let mut slides: Vec<(usize, String)> = names
        .lines()
        .filter_map(|n| {
            let num = n
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse::<usize>()
                .ok()?;
            Some((num, n.to_owned()))
        })
        .collect();
    slides.sort();
    let mut out = String::new();
    for (num, name) in slides.into_iter().take(MAX_SLIDES) {
        let mut read = Command::new("unzip");
        read.arg("-p").arg(path).arg(&name);
        let (_, xml) = run_tool(read, "unzip", EXTRACT_MAX_BYTES)?;
        let text = slide_text(&String::from_utf8_lossy(&xml));
        out.push_str(&format!("\u{c}── slide {num} ──\n{}\n", text.trim()));
        if out.len() as u64 > EXTRACT_MAX_BYTES {
            break;
        }
    }
    Ok(out)
}

/// The picture at `path` as pixels, shrunk with the system `sips` and read back as a bitmap.
///
/// # Errors
/// `sips` is missing, fails, runs too long, or the result is not a bitmap this reads.
pub fn load_image(path: &Path) -> Result<crate::art::Image, String> {
    let out = path.with_extension("preview.bmp");
    let mut command = Command::new("sips");
    command
        .args(["-s", "format", "bmp", "-Z", IMAGE_MAX_SIDE])
        .arg(path)
        .arg("--out")
        .arg(&out);
    let ran = run_tool(command, "sips", 64 * 1024);
    let bytes = std::fs::File::open(&out).ok().map(|f| {
        let mut buf = Vec::new();
        let _ = f.take(IMAGE_MAX_BYTES).read_to_end(&mut buf);
        buf
    });
    let _ = std::fs::remove_file(&out);
    ran?;
    let bytes = bytes.ok_or("could not read this picture")?;
    crate::art::parse_bmp(&bytes)
}

/// The text of a file, or a short reason it could not be read. A form feed marks a page or
/// slide break.
///
/// # Errors
/// The tool is missing, fails, or runs too long.
pub fn extract_text(path: &Path, kind: Kind) -> Result<String, String> {
    let (command, tool) = match kind {
        Kind::Pdf => {
            let mut c = Command::new("pdftotext");
            c.args(["-layout", "-q"]).arg(path).arg("-");
            (c, "pdftotext")
        }
        Kind::Word => {
            let mut c = Command::new("textutil");
            c.args(["-convert", "txt", "-stdout"]).arg(path);
            (c, "textutil")
        }
        Kind::Slides => {
            let text = slides_text(path)?;
            return if text.replace(['\u{c}', '\n'], "").trim().is_empty() {
                Err("no text in this file".to_owned())
            } else {
                Ok(text)
            };
        }
        Kind::Image => return Err("a picture has no text".to_owned()),
    };
    let (status, bytes) = run_tool(command, tool, EXTRACT_MAX_BYTES).map_err(|e| {
        if tool == "pdftotext" && e.contains("not available") {
            "pdftotext is not installed (brew install poppler)".to_owned()
        } else {
            e
        }
    })?;
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
    fn kinds_are_found_by_name_or_type() {
        assert_eq!(kind_of("a.PDF", ""), Some(Kind::Pdf));
        assert_eq!(kind_of("a.docx", ""), Some(Kind::Word));
        assert_eq!(kind_of("a.doc", ""), Some(Kind::Word));
        assert_eq!(kind_of("a.pptx", ""), Some(Kind::Slides));
        assert_eq!(kind_of("a.ppt", ""), Some(Kind::Slides));
        assert_eq!(kind_of("noext", "application/pdf"), Some(Kind::Pdf));
        assert_eq!(kind_of("a.png", "image/png"), Some(Kind::Image));
        assert_eq!(kind_of("image", "image/jpeg"), Some(Kind::Image));
        assert_eq!(kind_of("a.svg", "image/svg+xml"), None);
        assert_eq!(kind_of("a.xlsx", ""), None);
        assert_eq!(kind_of("a.txt", "text/plain"), None);
    }

    #[test]
    fn slide_text_reads_runs_and_paragraphs() {
        let xml = "<p:sld><a:p><a:r><a:t>Q3 &amp; Q4</a:t></a:r></a:p>\
                   <a:p><a:r><a:t xml:space=\"preserve\">plan</a:t></a:r><a:br/><a:r><a:t>x</a:t></a:r></a:p></p:sld>";
        assert_eq!(slide_text(xml), "Q3 & Q4\nplan\nx\n");
    }

    #[test]
    fn a_missing_file_is_a_short_reason_not_a_hang() {
        assert!(extract_text(Path::new("/no/such/file.pdf"), Kind::Pdf).is_err());
        assert!(extract_text(Path::new("/no/such/file.pptx"), Kind::Slides).is_err());
        assert!(load_image(Path::new("/no/such/file.png")).is_err());
    }
}
