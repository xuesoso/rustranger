//! In-pane image/document preview via terminal graphics (kitty / sixel).
//!
//! rustranger renders its UI as a diffed cell grid; images live *outside* that
//! grid. This manager runs a per-extension external renderer (see the `[preview]`
//! config — e.g. `folio print`) that writes graphics escapes to stdout, and emits
//! them over the preview box *after* each frame's cell flush. The preview cells
//! are left blank (see `ui::draw_preview_column`), so nothing fights the image.
//!
//! The renderer runs on a **background thread** so a slow rasterize (a large PDF)
//! never blocks cursor navigation: `sync` kicks off the render, returns
//! immediately, and paints the result on a later frame once it arrives — but only
//! if the selection still matches. A short debounce avoids spawning a renderer
//! for every file while scrolling. The image is deleted when the selection leaves
//! an image, an overlay covers the pane, the app suspends for an editor, or on quit.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crate::app::App;
use crate::config::PreviewProtocol;

/// folio's fixed kitty image id — the `[preview]` renderer uses it, so we delete
/// that id to clear. (Value: bytes of "pdf".)
const KITTY_IMAGE_ID: u32 = 0x0070_6466;

/// How long the selection must rest on a file before its preview starts, so fast
/// scrolling through a directory of images doesn't spawn a renderer per file.
const DEBOUNCE: Duration = Duration::from_millis(90);

pub type Rect = (u16, u16, u16, u16); // (x, top, cols, rows) in cells

/// What `sync` did this frame that the caller's cell-diff model must account for.
#[derive(Default)]
pub struct SyncOutcome {
    /// A preview-box rect that was blanked *directly on-screen* (outside the cell
    /// diff) while removing an image. The caller must blank the same cells in its
    /// diff baseline, so anything overlapping the box — e.g. a key-chain menu
    /// popup — is repainted next frame instead of staying hidden under the image
    /// we just erased.
    pub cleared: Option<Rect>,
    /// The removed image used sixel/iTerm2 pixels; force a full repaint so any
    /// pixels a cell overwrite didn't erase are cleaned up.
    pub needs_full: bool,
}

impl SyncOutcome {
    /// Fold a `clear()` result into the outcome. At most one clear does real work
    /// per `sync` (the first empties `shown`), so this records that rect.
    fn record_clear(&mut self, cleared: Option<(Rect, bool)>) {
        if let Some((rect, sixel)) = cleared {
            self.cleared = Some(rect);
            self.needs_full |= sixel;
        }
    }
}

struct Shown {
    path: PathBuf,
    rect: Rect,
    /// True if drawn with sixel or iTerm2 inline images: those pixels live in the
    /// cell grid, so clearing needs a full cell repaint (kitty has a delete cmd).
    sixel: bool,
    tmux: bool,
    /// Theme background at draw time — the box is repainted with it on clear, so
    /// transitions never flash the terminal's own background (the kitty-in-tmux
    /// placeholder cells are written outside our cell model with an unknown bg).
    bg: crossterm::style::Color,
}

/// A render running on a background thread; its result arrives over `rx`.
struct Inflight {
    path: PathBuf,
    rect: Rect,
    sixel: bool,
    tmux: bool,
    bg: crossterm::style::Color,
    rx: Receiver<Result<Vec<u8>, String>>,
}

#[derive(Default)]
pub struct ImagePreview {
    shown: Option<Shown>,
    /// A file whose preview is waiting out the debounce, and when it started.
    pending: Option<(PathBuf, Instant)>,
    /// A render currently running on a background thread.
    inflight: Option<Inflight>,
    /// The last (path, rect) a render was attempted for. Prevents an endless
    /// respawn loop when the renderer fails or emits nothing (e.g. not installed):
    /// each target is tried once until the selection or geometry changes.
    attempted: Option<(PathBuf, Rect)>,
}

impl ImagePreview {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while a debounced or in-flight render is outstanding — the caller
    /// should keep waking on a short tick so the image appears once it's ready.
    pub fn wants_tick(&self) -> bool {
        self.pending.is_some() || self.inflight.is_some()
    }

    /// Forget the displayed image without emitting anything (the real screen was
    /// wiped by a resize or a suspended full-screen child). Any in-flight render
    /// is abandoned; the next `sync` starts fresh.
    pub fn forget(&mut self) {
        self.shown = None;
        self.pending = None;
        self.inflight = None; // drop the receiver; the thread finishes, ignored
        self.attempted = None;
    }

    /// Reconcile the displayed image with the current selection, non-blocking.
    /// The returned [`SyncOutcome`] tells the caller how to keep its cell-diff
    /// baseline in step with what was drawn/erased directly on-screen.
    pub fn sync(
        &mut self,
        out: &mut impl Write,
        app: &mut App,
        cols: usize,
        rows: usize,
    ) -> io::Result<SyncOutcome> {
        let mut res = SyncOutcome::default();
        let desired = self.desired(app, cols, rows);

        // 1. Collect a finished background render and display it — but only if it
        //    still matches what we want (the cursor may have moved on).
        if let Some(inf) = &self.inflight {
            match inf.rx.try_recv() {
                Ok(result) => {
                    let inf = self.inflight.take().unwrap();
                    let still_wanted = desired
                        .as_ref()
                        .is_some_and(|(p, r)| *p == inf.path && *r == inf.rect);
                    if still_wanted {
                        match result {
                            Ok(bytes) if !bytes.is_empty() => {
                                res.record_clear(self.clear(out)?);
                                out.write_all(&bytes)?;
                                out.flush()?;
                                self.shown = Some(Shown {
                                    path: inf.path,
                                    rect: inf.rect,
                                    sixel: inf.sixel,
                                    tmux: inf.tmux,
                                    bg: inf.bg,
                                });
                            }
                            // Nothing to show (renderer missing) or it failed:
                            // remember the target so we don't respawn it every tick.
                            Ok(_) => self.attempted = Some((inf.path, inf.rect)),
                            Err(msg) => {
                                app.message = Some(format!("preview: {msg}"));
                                self.attempted = Some((inf.path, inf.rect));
                            }
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {} // still rendering
                Err(mpsc::TryRecvError::Disconnected) => self.inflight = None,
            }
        }

        // 2. Decide what to render/clear for the current selection.
        match desired {
            None => {
                self.pending = None;
                self.attempted = None; // a failed target may be retried on revisit
                res.record_clear(self.clear(out)?);
            }
            Some((path, rect)) => {
                if self.shown.as_ref().is_some_and(|s| s.path == path && s.rect == rect) {
                    self.pending = None;
                    return Ok(res);
                }
                // Remove a stale image now so nothing lingers while we render.
                res.record_clear(self.clear(out)?);
                // Already rendering exactly this — just wait for it.
                if self.inflight.as_ref().is_some_and(|i| i.path == path && i.rect == rect) {
                    self.pending = None;
                    return Ok(res);
                }
                // Already tried this exact target and it produced nothing (failed
                // or renderer missing) — don't spin retrying it every tick.
                if self.attempted.as_ref().is_some_and(|(p, r)| *p == path && *r == rect) {
                    self.pending = None;
                    return Ok(res);
                }
                // Debounce, then kick off a background render.
                let ready = matches!(&self.pending, Some((p, since))
                    if *p == path && since.elapsed() >= DEBOUNCE);
                if ready {
                    self.pending = None;
                    self.spawn(app, path, rect);
                } else if !matches!(&self.pending, Some((p, _)) if *p == path) {
                    self.pending = Some((path, Instant::now()));
                }
            }
        }
        Ok(res)
    }

    /// Remove the displayed image, if any. Returns the box rect that was blanked
    /// on-screen and whether the image used sixel/iTerm2 pixels (so the caller can
    /// re-sync its diff baseline and, for sixel, force a full repaint). `None`
    /// when nothing was shown.
    pub fn clear(&mut self, out: &mut impl Write) -> io::Result<Option<(Rect, bool)>> {
        use crossterm::cursor::MoveTo;
        use crossterm::style::{Print, ResetColor, SetBackgroundColor};

        let Some(s) = self.shown.take() else {
            return Ok(None);
        };
        let mut seq: Vec<u8> = Vec::new();
        // Kitty: delete the image by id (a clean overlay removal). Harmless when
        // the image was sixel (there is no such kitty image).
        let del = format!("\x1b_Ga=d,d=I,i={KITTY_IMAGE_ID}\x1b\\");
        seq.extend_from_slice(&maybe_tmux(del.as_bytes(), s.tmux));
        // Repaint the box with the theme background so the transition never
        // flashes the terminal's own background: the kitty-in-tmux placeholder
        // cells were written outside our cell model (with an unknown bg), and
        // sixel/iterm pixels are erased by cell writes in most terminals. This
        // matches the diff model (blank theme-bg cells), so it stays consistent.
        let (x, top, cols, rows) = s.rect;
        let blank = " ".repeat(cols as usize);
        let _ = crossterm::queue!(seq, SetBackgroundColor(s.bg));
        for r in 0..rows {
            let _ = crossterm::queue!(seq, MoveTo(x, top + r), Print(&blank));
        }
        let _ = crossterm::queue!(seq, ResetColor);
        out.write_all(&seq)?;
        out.flush()?;
        // Sixel/iterm pixels may survive cell overwrites on some terminals; keep
        // the full repaint as the reliable eraser there. Kitty needs none.
        Ok(Some((s.rect, s.sixel)))
    }

    /// Which image (path + preview-box rect) should be showing right now, if any.
    fn desired(&self, app: &App, cols: usize, rows: usize) -> Option<(PathBuf, Rect)> {
        // No image while an overlay covers the browser view.
        if app.help.is_some() || app.menu.is_some() || app.console.is_some() {
            return None;
        }
        let entry = app.current_dir().current()?;
        if !matches!(entry.ftype, crate::fs::FType::File) || !app.is_image_preview(&entry.path) {
            return None;
        }
        let rect = crate::ui::preview_rect(cols, rows, &app.settings.column_ratios)?;
        Some((entry.path.clone(), rect))
    }

    /// Start rendering `path` on a background thread (returns immediately).
    fn spawn(&mut self, app: &App, path: PathBuf, rect: Rect) {
        let Some(tmpl) = app.preview_command_for(&path).map(str::to_string) else {
            return;
        };
        let proto = resolve_protocol(app.settings.preview_protocol);
        let (cell_w, cell_h) = cell_pixel_size();
        let tmux = in_tmux();
        let bg = theme_bg_hex(app);
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        thread::spawn(move || {
            let _ = tx.send(run_renderer(&tmpl, &p, rect, proto, cell_w, cell_h, tmux, &bg));
        });
        self.inflight = Some(Inflight {
            path,
            rect,
            sixel: proto != "kitty", // sixel + iterm both clear by repaint
            tmux,
            bg: app.settings.theme.bg,
            rx,
        });
    }
}

/// Run the renderer command (placeholders substituted) and capture its stdout.
#[allow(clippy::too_many_arguments)]
fn run_renderer(
    template: &str,
    path: &Path,
    rect: Rect,
    proto: &str,
    cell_w: u16,
    cell_h: u16,
    tmux: bool,
    bg: &str,
) -> Result<Vec<u8>, String> {
    let (x, y, c, r) = rect;
    let path_str = path.to_string_lossy();
    // Substitute per whitespace-token so a `%f` path with spaces stays one arg,
    // and `%t` can drop out entirely when not in tmux.
    let argv: Vec<String> = template
        .split_whitespace()
        .map(|tok| {
            tok.replace("%f", &path_str)
                .replace("%p", proto)
                .replace("%x", &x.to_string())
                .replace("%y", &y.to_string())
                .replace("%c", &c.to_string())
                .replace("%r", &r.to_string())
                .replace("%w", &cell_w.to_string())
                .replace("%h", &cell_h.to_string())
                .replace("%b", bg)
                .replace("%t", if tmux { "--tmux" } else { "" })
        })
        .filter(|s| !s.is_empty())
        .collect();
    let Some((prog, args)) = argv.split_first() else {
        return Err("empty preview command".to_string());
    };
    // Optional diagnostics: RUSTRANGER_PREVIEW_LOG=<file> appends every renderer
    // invocation and its outcome, for debugging terminal/protocol issues.
    let log = |line: &str| {
        if let Some(p) = std::env::var_os("RUSTRANGER_PREVIEW_LOG") {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                let _ = writeln!(f, "{line}");
            }
        }
    };
    log(&format!("run: {argv:?}"));
    let output = match Command::new(prog).args(args).output() {
        Ok(o) => o,
        // Renderer isn't installed / not on PATH: skip the preview silently rather
        // than spamming the status bar on every image (the default config ships a
        // `folio` command that not everyone has).
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            log("  -> renderer not found; skipping");
            return Ok(Vec::new());
        }
        Err(e) => {
            log(&format!("  -> spawn error: {e}"));
            return Err(format!("{prog}: {e}"));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log(&format!("  -> exit {:?}, stderr: {}", output.status.code(), stderr.trim()));
        return Err(stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("renderer failed")
            .to_string());
    }
    log(&format!("  -> ok, {} bytes of escapes", output.stdout.len()));
    Ok(output.stdout)
}

/// Resolve the configured protocol to the string passed to the renderer (`%p`).
fn resolve_protocol(p: PreviewProtocol) -> &'static str {
    match p {
        PreviewProtocol::Kitty => "kitty",
        PreviewProtocol::Sixel => "sixel",
        PreviewProtocol::Iterm => "iterm",
        PreviewProtocol::Auto => {
            if in_tmux() {
                // Env vars are STICKY across tmux attaches (the server keeps the
                // env of wherever it started), so they lie about the terminal
                // currently attached. Ask the tmux server instead: it knows the
                // active client's TERM. kitty/Ghostty get the placeholder-based
                // kitty path (the only race-free mechanism); WezTerm gets OSC
                // 1337 (its kitty support lacks placeholders); everything else —
                // including iTerm2 — goes through tmux's native sixel, which is
                // the path tmux itself keeps consistent for such clients.
                match tmux_client_term().as_deref() {
                    Some(t) if t.contains("ghostty") || t.contains("kitty") => "kitty",
                    Some(t) if t.contains("wezterm") => "iterm",
                    _ => "sixel",
                }
            } else if kitty_capable() || wezterm() {
                // Direct (no tmux): WezTerm's kitty support is fine here.
                "kitty"
            } else if iterm_capable() || vscode() {
                // iTerm2 / VSCode render OSC 1337 inline images directly.
                "iterm"
            } else {
                "sixel"
            }
        }
    }
}

/// Best-effort detection of a fully kitty-graphics-capable terminal — one that
/// also renders the Unicode placeholders needed inside tmux (kitty, Ghostty).
/// Env-based, so it survives tmux where a capability query can't be issued.
fn kitty_capable() -> bool {
    let env = |k: &str| std::env::var_os(k).is_some();
    env("KITTY_WINDOW_ID")
        || env("GHOSTTY_RESOURCES_DIR")
        || env("GHOSTTY_BIN_DIR")
        || matches!(
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            Some("ghostty") | Some("kitty")
        )
        || std::env::var("TERM").is_ok_and(|t| t.contains("kitty"))
}

/// WezTerm: kitty graphics without Unicode placeholders, plus OSC 1337.
fn wezterm() -> bool {
    std::env::var_os("WEZTERM_EXECUTABLE").is_some()
        || matches!(std::env::var("TERM_PROGRAM").ok().as_deref(), Some("WezTerm"))
}

/// VSCode's integrated terminal (renders OSC 1337 when images are enabled).
fn vscode() -> bool {
    matches!(std::env::var("TERM_PROGRAM").ok().as_deref(), Some("vscode"))
}

/// The theme background as an `RRGGBB` hex string for the `%b` placeholder, or
/// "none" when the theme uses the terminal's own background (Reset / named /
/// indexed colors have no portable RGB value to hand a renderer).
fn theme_bg_hex(app: &App) -> String {
    match app.settings.theme.bg {
        crossterm::style::Color::Rgb { r, g, b } => format!("{r:02x}{g:02x}{b:02x}"),
        _ => "none".to_string(),
    }
}

/// Detection of iTerm2 (env-based; `LC_TERMINAL` covers ssh sessions).
fn iterm_capable() -> bool {
    std::env::var_os("ITERM_SESSION_ID").is_some()
        || matches!(std::env::var("TERM_PROGRAM").ok().as_deref(), Some("iTerm.app"))
        || matches!(std::env::var("LC_TERMINAL").ok().as_deref(), Some("iTerm2"))
}

fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Ask tmux for a format expansion, targeted at OUR pane's session so the answer
/// reflects the client actually viewing this pane (a server can have several
/// sessions/clients attached from different terminals).
fn tmux_display(fmt: &str) -> Option<String> {
    let mut cmd = Command::new("tmux");
    cmd.arg("display-message");
    if let Some(pane) = std::env::var_os("TMUX_PANE") {
        cmd.arg("-t").arg(pane);
    }
    let out = cmd.args(["-p", fmt]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// TERM of the client currently attached to the tmux session (lowercased), e.g.
/// "xterm-ghostty" / "xterm-kitty" / "xterm-256color". Unlike pane env vars,
/// this tracks re-attaching from a different terminal emulator.
fn tmux_client_term() -> Option<String> {
    tmux_display("#{client_termname}").map(|s| s.to_lowercase())
}

/// Pixel size of one cell. Inside tmux the pane pty reports no pixel size, but
/// the tmux server knows the attached client's cell size exactly — using it
/// keeps the rendered canvas aligned to real cell boundaries (a wrong guess
/// leaves sub-cell slivers at the image's right/bottom edges that terminals
/// fill with the sixel background, i.e. white strips). Outside tmux, TIOCGWINSZ;
/// falls back to a common 8×16.
fn cell_pixel_size() -> (u16, u16) {
    if in_tmux() {
        if let Some(wh) = tmux_cell_size() {
            return wh;
        }
    }
    if let Ok(ws) = crossterm::terminal::window_size() {
        if ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0 {
            return (ws.width / ws.columns, ws.height / ws.rows);
        }
    }
    (8, 16)
}

/// The attached tmux client's cell size in pixels (tracks re-attaching from a
/// different terminal, like [`tmux_client_term`]).
fn tmux_cell_size() -> Option<(u16, u16)> {
    let s = tmux_display("#{client_cell_width} #{client_cell_height}")?;
    let mut it = s.split_whitespace();
    let w: u16 = it.next()?.parse().ok()?;
    let h: u16 = it.next()?.parse().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// Wrap a control sequence for tmux passthrough when `tmux` is set (double each
/// `ESC`, frame with `ESC P tmux; … ESC \`). Requires tmux `allow-passthrough`.
fn maybe_tmux(seq: &[u8], tmux: bool) -> Vec<u8> {
    if !tmux {
        return seq.to_vec();
    }
    let mut out = Vec::with_capacity(seq.len() + 16);
    out.extend_from_slice(b"\x1bPtmux;");
    for &b in seq {
        if b == 0x1b {
            out.push(0x1b);
        }
        out.push(b);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::style::Color;

    fn shown(rect: Rect, sixel: bool) -> Shown {
        Shown { path: PathBuf::from("/img.png"), rect, sixel, tmux: false, bg: Color::Reset }
    }

    #[test]
    fn clear_reports_blanked_rect_and_forgets_image() {
        let mut ip = ImagePreview::new();
        ip.shown = Some(shown((10, 1, 20, 15), false));
        let mut out: Vec<u8> = Vec::new();

        // The rect is reported so the caller can re-sync its diff baseline (this
        // is what stops a menu popup over the preview box from staying hidden).
        assert_eq!(ip.clear(&mut out).unwrap(), Some(((10, 1, 20, 15), false)));
        assert!(ip.shown.is_none(), "image forgotten after clear");
        // Nothing shown now → nothing to re-sync.
        assert_eq!(ip.clear(&mut out).unwrap(), None);
    }

    #[test]
    fn clear_flags_sixel_for_full_repaint_but_not_kitty() {
        let mut out: Vec<u8> = Vec::new();

        let mut kitty = ImagePreview::new();
        kitty.shown = Some(shown((0, 0, 4, 4), false));
        assert_eq!(kitty.clear(&mut out).unwrap().map(|(_, s)| s), Some(false));

        let mut sixel = ImagePreview::new();
        sixel.shown = Some(shown((0, 0, 4, 4), true));
        assert_eq!(sixel.clear(&mut out).unwrap().map(|(_, s)| s), Some(true));
    }

    #[test]
    fn sync_outcome_folds_clear_results() {
        let mut r = SyncOutcome::default();
        r.record_clear(None);
        assert!(r.cleared.is_none() && !r.needs_full);

        // A kitty clear records the rect without demanding a full repaint.
        r.record_clear(Some(((1, 2, 3, 4), false)));
        assert_eq!(r.cleared, Some((1, 2, 3, 4)));
        assert!(!r.needs_full);

        // A sixel clear escalates to a full repaint.
        r.record_clear(Some(((5, 6, 7, 8), true)));
        assert!(r.needs_full);
    }
}
