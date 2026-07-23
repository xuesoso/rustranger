//! Copy text to the system clipboard by shelling out to the platform tool
//! (`pbcopy` on macOS; `wl-copy` / `xclip` / `xsel` on Linux). No extra deps.

use std::io::{self, Write};
use std::process::{Command, Stdio};

/// Copy `text` to the system clipboard. Tries the platform tools in order and
/// returns on the first success, or an error if none is available.
pub fn copy(text: &str) -> io::Result<()> {
    let candidates: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        &[
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ]
    };
    let mut last: Option<io::Error> = None;
    for argv in candidates {
        match feed(argv, text) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no clipboard tool found")))
}

/// Spawn `argv` and write `text` to its stdin.
fn feed(argv: &[&str], text: &str) -> io::Result<()> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
        // stdin drops here, closing the pipe so the tool reads EOF and exits.
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{} exited with {}", argv[0], status)))
    }
}
