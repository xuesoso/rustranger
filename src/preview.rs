// Native text-file preview. No scope.sh / external helpers: we read the head of
// the file, sniff for binary content, and expose it as sanitized lines.
// Mirrors the intent of ranger/container/file.py has_preview()/is_binary().

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// How much of a file we read for a preview.
const PREVIEW_BYTES: usize = 64 * 1024;
/// Files larger than this are not previewed at all.
const MAX_PREVIEW_SIZE: u64 = 16 * 1024 * 1024;
/// How many bytes of the head we scan when sniffing for binary content.
const SNIFF_BYTES: usize = 8192;

/// Extensions we never text-preview: documents, images, media, archives, and
/// compiled blobs. PDFs and images in particular have ASCII headers that fool a
/// byte sniff, then spew their binary streams across the screen. These are the
/// formats a future image/document backend (see `crate::image`) would handle;
/// until then they simply show "no preview".
const BINARY_EXTS: &[&str] = &[
    // documents
    "pdf", "ps", "eps", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "epub",
    "mobi", "azw3", "djvu",
    // images (would be routed to image display once implemented)
    "png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp", "ico", "heic", "heif", "avif",
    "psd", "xcf",
    // audio / video
    "mp3", "m4a", "flac", "wav", "ogg", "opus", "aac", "wma", "mp4", "m4v", "mkv", "avi", "mov",
    "wmv", "flv", "webm", "mpg", "mpeg",
    // archives
    "zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "lz", "lzma", "cab", "ar",
    // binaries / data blobs
    "exe", "dll", "so", "o", "a", "dylib", "bin", "class", "jar", "pyc", "wasm", "sqlite", "db",
    "iso", "img", "dmg", "deb", "rpm",
    // fonts
    "woff", "woff2", "ttf", "otf", "eot",
];

#[derive(Clone)]
pub enum Preview {
    Text(Vec<String>),
    Binary,
    TooBig,
    Empty,
    Error(String),
}

pub fn load(path: &Path, size: u64) -> Preview {
    if size == 0 {
        return Preview::Empty;
    }
    if size > MAX_PREVIEW_SIZE {
        return Preview::TooBig;
    }
    // Known binary/image/document types are never text-previewed.
    if has_binary_extension(path) {
        return Preview::Binary;
    }

    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return Preview::Error(e.to_string()),
    };

    let mut buf = vec![0u8; PREVIEW_BYTES];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(e) => return Preview::Error(e.to_string()),
    };
    buf.truncate(n);

    if is_binary(&buf) {
        return Preview::Binary;
    }

    // Decode lossily so invalid UTF-8 still previews instead of failing, then
    // sanitize every line so no control/escape byte can reach the terminal.
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<String> = text.split('\n').map(sanitize_line).collect();
    Preview::Text(lines)
}

fn has_binary_extension(path: &Path) -> bool {
    match path.extension() {
        Some(ext) => {
            let ext = ext.to_string_lossy().to_lowercase();
            BINARY_EXTS.contains(&ext.as_str())
        }
        None => false,
    }
}

/// Render a line safe for the terminal: tabs become spaces, and every other
/// control character (including ESC, which drives escape sequences) becomes a
/// visible middle dot. This guarantees a misdetected binary can corrupt the
/// listing at worst into dots, never into cursor moves / color spew.
fn sanitize_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        match c {
            '\t' => out.push_str("    "),
            '\r' => {} // drop stray carriage returns
            c if c.is_control() => out.push('\u{00B7}'),
            c => out.push(c),
        }
    }
    out
}

/// Heuristic binary check: a NUL byte, or too many control bytes (including ESC)
/// in the sampled head, marks the file as binary.
fn is_binary(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return false;
    }
    let sample = &buf[..buf.len().min(SNIFF_BYTES)];
    let mut suspicious = 0usize;
    for &b in sample {
        if b == 0 {
            return true;
        }
        // Printable + the common text whitespace (tab/newline/CR/form-feed/VT)
        // are fine; anything else in the control range is suspicious — including
        // ESC (0x1b), which legitimate plain text effectively never contains.
        let is_text_ws = matches!(b, b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
        if (b < 0x20 && !is_text_ws) || b == 0x7f {
            suspicious += 1;
        }
    }
    suspicious * 100 / sample.len() > 10
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn pdf_is_not_text_previewed() {
        let dir = std::env::temp_dir().join(format!("rr_prev_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let pdf: PathBuf = dir.join("doc.pdf");
        // An ASCII header that would otherwise pass the byte sniff.
        let body = b"%PDF-1.7\n1 0 obj<< /Type /Catalog >>endobj\n";
        std::fs::write(&pdf, body).unwrap();
        assert!(matches!(load(&pdf, body.len() as u64), Preview::Binary));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn control_bytes_are_sanitized() {
        // ESC and other control chars must not survive into preview output.
        let s = "ok\x1b[31mred\x07\x00end";
        let out = sanitize_line(s);
        assert!(!out.contains('\x1b'));
        assert!(!out.contains('\x07'));
        assert!(out.contains("ok") && out.contains("red") && out.contains("end"));
    }

    #[test]
    fn plain_text_previews() {
        assert!(!is_binary(b"hello world\nsecond line\n"));
        assert!(is_binary(b"text\x00\x00binary"));
    }
}
