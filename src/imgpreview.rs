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
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::app::App;
use crate::config::PreviewProtocol;

/// folio's fixed kitty image id — the `[preview]` renderer uses it, so we delete
/// that id to clear. (Value: bytes of "pdf".)
const KITTY_IMAGE_ID: u32 = 0x0070_6466;

/// Process-group ids of renderers running right now.
///
/// The orderly shutdown path is [`ImagePreview::drop`], which cancels the render
/// and waits for the thread to reap its child. That covers quitting and panics,
/// but not `_exit(2)` — the hangup watchdog's last resort — which runs no
/// destructors at all. Without this registry a forced exit would strand a
/// half-finished `folio` rasterizing a large page: reparented to init, invisible,
/// burning a core, exactly the orphan the watchdog exists to prevent.
static LIVE_RENDERERS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Registers a renderer for the lifetime of [`run_renderer`], so it is removed
/// however that function returns (success, timeout, cancel, or error).
struct Registered(u32);

impl Registered {
    fn new(pid: u32) -> Registered {
        if let Ok(mut live) = LIVE_RENDERERS.lock() {
            live.push(pid);
        }
        Registered(pid)
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        if let Ok(mut live) = LIVE_RENDERERS.lock() {
            live.retain(|&p| p != self.0);
        }
    }
}

/// Kill every renderer still running, immediately and without waiting.
///
/// For use on paths that will not run destructors — the watchdog's forced exit.
/// Each renderer got a process group of its own at spawn, so the negated pid
/// takes any helpers a shell-wrapped `[preview]` command started as well.
/// `try_lock` because this runs when the process is already being abandoned:
/// a registry busy for a moment is not worth deadlocking the exit over.
pub fn kill_live_renderers() {
    if let Ok(live) = LIVE_RENDERERS.try_lock() {
        for &pid in live.iter() {
            // SAFETY: kill(2) with a negated pid signals that process group;
            // a stale pid can only fail with ESRCH, which we ignore.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        }
    }
}

/// How long the selection must rest on a file before its preview starts, so fast
/// scrolling through a directory of images doesn't spawn a renderer per file.
const DEBOUNCE: Duration = Duration::from_millis(90);

/// Give up on a renderer that hasn't finished in this long and kill it. A
/// renderer that never returns would otherwise pin the run loop to its 50 ms
/// "a preview is coming" tick forever (10x the idle wake-up rate, for good), and
/// go on burning CPU as an orphan once rustranger exits.
const RENDER_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to sit in `poll(2)` between checks of the cancel flag while a
/// renderer is running. Short enough that navigating away kills the renderer
/// promptly, long enough to cost nothing.
const POLL_SLICE_MS: libc::c_int = 50;

/// How long a cached answer from the tmux server stays good. The cache is also
/// dropped outright on a resize (see [`ImagePreview::forget`]), which is what a
/// client attaching, detaching, or re-attaching from a different terminal
/// normally causes; this bounds the case where it doesn't.
const TMUX_TTL: Duration = Duration::from_secs(5);

/// How long to wait before looking again after skipping a render because no tmux
/// client was attached. Attaching usually resizes the pane and clears everything,
/// but a client that happens to match the session's existing size doesn't — so
/// re-check occasionally rather than waiting for the next keystroke.
const DETACHED_RETRY: Duration = Duration::from_secs(2);

/// Ceiling on a `tmux display-message` round-trip. It normally answers in ~3 ms;
/// this runs on the UI thread, so a server that is wedged, paused, or gone must
/// cost us a hitch rather than the session.
const TMUX_TIMEOUT: Duration = Duration::from_secs(1);

/// After killing a renderer, how long to keep reading before giving up on its
/// pipes reaching EOF. A killed process closes them at once, but if it left
/// children of its own holding the write end (a shell wrapper that didn't
/// `exec`), waiting for EOF would mean waiting for *them* — the very hang we are
/// trying to escape.
const KILL_GRACE: Duration = Duration::from_millis(250);

/// How long shutdown waits for an abandoned renderer to actually die. Cancelling
/// only *asks* the render thread to kill its child, and a process that exits the
/// moment after has no threads left to do the asking — so the last one out waits
/// for the acknowledgement rather than leaving an orphan behind.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(500);

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

/// Cancels the render it belongs to when dropped.
///
/// Dropping an [`Inflight`] is how rustranger says "I no longer want this
/// picture" — the selection moved on, an overlay opened, the screen was wiped,
/// or we're quitting. Without this the renderer would keep running unwatched:
/// on quit it outlives us entirely, reparented to init, still rasterizing a page
/// nobody will ever see. `Inflight` itself deliberately has no `Drop` impl, so
/// its other fields can still be moved out; this field runs regardless.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// A render running on a background thread; its result arrives over `rx`.
struct Inflight {
    path: PathBuf,
    rect: Rect,
    sixel: bool,
    tmux: bool,
    bg: crossterm::style::Color,
    rx: Receiver<Result<Vec<u8>, String>>,
    /// Dropped when this render is abandoned or superseded, which kills the
    /// renderer process (see [`CancelOnDrop`]).
    _cancel: CancelOnDrop,
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
    /// When a render was last skipped for want of an attached tmux client, so the
    /// skip can lapse instead of lasting until the next keypress.
    detached_at: Option<Instant>,
}

/// Kill any renderer still running when the preview manager goes away — that is,
/// when rustranger is quitting. Dropping the [`Inflight`] alone only sets the
/// cancel flag; the render thread that acts on it dies with the process, so
/// without this wait a `folio` rasterizing a large page would be left running,
/// reparented to init, invisible and burning a core until noticed by hand.
impl Drop for ImagePreview {
    fn drop(&mut self) {
        let Some(inf) = self.inflight.take() else {
            return;
        };
        inf._cancel.0.store(true, Ordering::Relaxed);
        // The render thread only reports back once it has killed and reaped the
        // renderer, so this returning means the child is gone. On timeout we quit
        // anyway: a dying pipe will finish the job for any renderer that writes.
        let _ = inf.rx.recv_timeout(SHUTDOWN_WAIT);
    }
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
        self.detached_at = None;
        // A resize is the one event that reliably accompanies a tmux client
        // attaching, detaching, or re-attaching from a different terminal — and
        // each of those changes the protocol and cell size we render for.
        invalidate_tmux_client();
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
                // A render skipped for want of a tmux client gets another look
                // once in a while; see DETACHED_RETRY.
                if self.detached_at.is_some_and(|t| t.elapsed() >= DETACHED_RETRY) {
                    self.detached_at = None;
                    self.attempted = None;
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
        // Inside tmux with no client attached there is nobody to show the image
        // to — and no client to measure, so the cell size would be a guess and
        // the page would rasterize at the wrong resolution anyway. Skip it, and
        // note the skip so it can lapse (attaching normally resizes the pane,
        // which clears this outright).
        if in_tmux() && !tmux_client().attached {
            self.attempted = Some((path, rect));
            self.detached_at = Some(Instant::now());
            return;
        }
        let proto = resolve_protocol(app.settings.preview_protocol);
        let (cell_w, cell_h) = cell_pixel_size();
        let tmux = in_tmux();
        let bg = theme_bg_hex(app);
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        thread::spawn(move || {
            let _ = tx.send(run_renderer(&tmpl, &p, rect, proto, cell_w, cell_h, tmux, &bg, &flag));
        });
        // Assigning replaces any previous Inflight, whose CancelOnDrop stops the
        // render it stood for — so a burst of navigation leaves one renderer
        // running, not one per file passed through.
        self.inflight = Some(Inflight {
            path,
            rect,
            sixel: proto != "kitty", // sixel + iterm both clear by repaint
            tmux,
            bg: app.settings.theme.bg,
            rx,
            _cancel: CancelOnDrop(cancel),
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
    cancel: &AtomicBool,
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
    // stdin is closed: a renderer must never read the keystrokes meant for us.
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Give the renderer a process group of its own, so cancelling it takes any
    // helpers it spawned with it (a `[preview]` command may well be a shell
    // wrapper) instead of orphaning them. It also keeps terminal signals meant
    // for the foreground app from reaching a background helper.
    // SAFETY: the closure runs between fork and exec and calls only setpgid,
    // which is async-signal-safe.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let spawned = cmd.spawn();
    let mut child = match spawned {
        Ok(c) => c,
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
    // Track it until this function returns, so a forced exit (which runs no
    // destructors) can still take the renderer with it.
    let _registered = Registered::new(child.id());
    let collected = collect(&mut child, cancel, RENDER_TIMEOUT);
    let (stdout, stderr, status) = match collected {
        Ok(v) => v,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            log(&format!("  -> read error: {e}"));
            return Err(format!("{prog}: {e}"));
        }
    };
    if let Outcome::Cancelled = status {
        // We asked for this: the selection moved on, or we're shutting down.
        log("  -> abandoned; renderer killed");
        return Ok(Vec::new());
    }
    if let Outcome::TimedOut = status {
        let secs = RENDER_TIMEOUT.as_secs();
        log(&format!("  -> timed out after {secs}s; renderer killed"));
        return Err(format!("{prog}: no output after {secs}s"));
    }
    let Outcome::Exited(exit) = status else { unreachable!() };
    if !exit.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        log(&format!("  -> exit {:?}, stderr: {}", exit.code(), stderr.trim()));
        return Err(stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("renderer failed")
            .to_string());
    }
    log(&format!("  -> ok, {} bytes of escapes", stdout.len()));
    Ok(stdout)
}

/// How a renderer run ended.
enum Outcome {
    Exited(std::process::ExitStatus),
    /// Abandoned mid-render (the cancel flag was set) and killed.
    Cancelled,
    /// Still running at [`RENDER_TIMEOUT`] and killed.
    TimedOut,
}

/// Run `child` to completion, collecting stdout and stderr — but kill it if
/// `cancel` is set or `timeout` elapses first, rather than letting a renderer we
/// no longer care about run on unsupervised.
///
/// Both pipes are drained as they fill. That is not optional: a full-page sixel
/// is comfortably larger than a pipe buffer, so a renderer whose stdout nobody
/// reads simply blocks forever — the deadlock `Command::output` exists to avoid,
/// and which we have to reproduce by hand now that we also want to interrupt it.
fn collect(
    child: &mut Child,
    cancel: &AtomicBool,
    timeout: Duration,
) -> io::Result<(Vec<u8>, Vec<u8>, Outcome)> {
    use std::os::fd::AsRawFd;

    let (out_pipe, err_pipe) = (child.stdout.take(), child.stderr.take());
    let mut fd_out = out_pipe.as_ref().map_or(-1, |p| p.as_raw_fd());
    let mut fd_err = err_pipe.as_ref().map_or(-1, |p| p.as_raw_fd());
    for fd in [fd_out, fd_err] {
        // SAFETY: fd is a live pipe fd owned by `child` for this scope.
        unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
    }

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut buf = [0u8; 16 * 1024];
    let start = Instant::now();
    let mut killed: Option<Outcome> = None;
    let mut give_up_at: Option<Instant> = None;

    while fd_out >= 0 || fd_err >= 0 {
        if killed.is_none() {
            let reason = if cancel.load(Ordering::Relaxed) {
                Some(Outcome::Cancelled)
            } else if start.elapsed() >= timeout {
                Some(Outcome::TimedOut)
            } else {
                None
            };
            if let Some(r) = reason {
                // The whole group: the renderer and anything it started.
                // SAFETY: kill(2) with a negative pid signals that process
                // group; the child is unreaped, so its pid is still ours.
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                let _ = child.kill(); // in case it never got its own group
                killed = Some(r);
                give_up_at = Some(Instant::now() + KILL_GRACE);
            }
        }
        if give_up_at.is_some_and(|t| Instant::now() >= t) {
            break; // stop waiting on pipes something else may still hold open
        }
        let mut pfds = [
            libc::pollfd { fd: fd_out, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: fd_err, events: libc::POLLIN, revents: 0 },
        ];
        // A negative fd makes poll skip that entry, which is how a pipe that has
        // already reached EOF drops out of the wait.
        // SAFETY: pfds holds two valid, initialised pollfds.
        let rc = unsafe { libc::poll(pfds.as_mut_ptr(), 2, POLL_SLICE_MS) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        for (i, (fd, sink)) in [(&mut fd_out, &mut out), (&mut fd_err, &mut err)]
            .into_iter()
            .enumerate()
        {
            // Any revent (POLLIN, but also POLLHUP/POLLERR) means "read it and
            // find out"; a hung-up pipe simply reads as EOF.
            if *fd < 0 || pfds[i].revents == 0 {
                continue;
            }
            loop {
                // SAFETY: buf is a valid writable buffer of buf.len() bytes.
                let n = unsafe { libc::read(*fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n > 0 {
                    sink.extend_from_slice(&buf[..n as usize]);
                    continue;
                }
                if n == 0 {
                    *fd = -1; // EOF: this pipe is done
                    break;
                }
                match io::Error::last_os_error().kind() {
                    io::ErrorKind::WouldBlock => break,
                    io::ErrorKind::Interrupted => continue,
                    _ => {
                        *fd = -1;
                        break;
                    }
                }
            }
        }
    }
    drop((out_pipe, err_pipe));
    let exit = child.wait()?;
    Ok((out, err, killed.unwrap_or(Outcome::Exited(exit))))
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
                match tmux_client().term.as_deref() {
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

/// Run a short helper command and return its stdout, or `None` if it failed or
/// overran `timeout`. Uses the same bounded, own-process-group machinery as the
/// renderer: this runs on the UI thread, so it must not be able to hang there,
/// and it must not leave a child behind if it does.
fn run_bounded(mut cmd: Command, timeout: Duration) -> Option<Vec<u8>> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: the closure runs between fork and exec and calls only setpgid,
    // which is async-signal-safe.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = cmd.spawn().ok()?;
    // Registered like a renderer: this runs on the UI thread, so a forced exit
    // can land while the query is still in flight.
    let _registered = Registered::new(child.id());
    let (out, _, outcome) = collect(&mut child, &AtomicBool::new(false), timeout).ok()?;
    matches!(outcome, Outcome::Exited(s) if s.success()).then_some(out)
}

fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// What the tmux server says about the client currently viewing our pane.
///
/// Everything here is a property of the *client*, not the pane: a session can be
/// attached from several terminals, or from none, and re-attaching from a
/// different emulator changes all of it. Pane environment variables are stale by
/// construction (the server keeps the env of wherever it was started), so the
/// server is the only honest source.
#[derive(Clone, Default)]
struct TmuxClient {
    /// Whether anyone is attached at all. Nothing rendered while this is false
    /// can be seen by anybody.
    attached: bool,
    /// TERM of the attached client (lowercased), e.g. "xterm-ghostty".
    term: Option<String>,
    /// The client's cell size in pixels.
    cell: Option<(u16, u16)>,
}

/// Cached [`TmuxClient`], with the time it was fetched.
static TMUX_CLIENT: std::sync::Mutex<Option<(Instant, TmuxClient)>> = std::sync::Mutex::new(None);

/// Drop the cached answer, so the next question goes to the server.
fn invalidate_tmux_client() {
    if let Ok(mut g) = TMUX_CLIENT.lock() {
        *g = None;
    }
}

/// The attached client's properties, from cache when it is fresh.
///
/// Every miss is a `tmux display-message` round trip: a fork, an exec, and a
/// wait on the socket, about 3 ms — on the UI thread. Asking twice per preview
/// (once for the protocol, once for the cell size) made every image cost ~6 ms
/// of stall before the renderer even started; one combined query, cached, makes
/// that nearly always free.
fn tmux_client() -> TmuxClient {
    if let Ok(g) = TMUX_CLIENT.lock() {
        if let Some((at, facts)) = g.as_ref() {
            if at.elapsed() < TMUX_TTL {
                return facts.clone();
            }
        }
    }
    let facts = probe_tmux_client();
    if let Ok(mut g) = TMUX_CLIENT.lock() {
        *g = Some((Instant::now(), facts.clone()));
    }
    facts
}

/// One round trip for everything we need. Targeted at OUR pane's session, so the
/// answer describes the client actually viewing this pane.
fn probe_tmux_client() -> TmuxClient {
    const FMT: &str =
        "#{session_attached}|#{client_termname}|#{client_cell_width}|#{client_cell_height}";
    let mut cmd = Command::new("tmux");
    cmd.arg("display-message");
    if let Some(pane) = std::env::var_os("TMUX_PANE") {
        cmd.arg("-t").arg(pane);
    }
    cmd.args(["-p", FMT]);
    let Some(out) = run_bounded(cmd, TMUX_TIMEOUT) else {
        return TmuxClient::default();
    };
    parse_tmux_client(&String::from_utf8_lossy(&out))
}

/// Parse the reply to the format above: `attached|term|cell_w|cell_h`. Every
/// field but the first is empty when nobody is attached, and older servers may
/// not know the cell-size formats at all — so each is optional on its own.
fn parse_tmux_client(text: &str) -> TmuxClient {
    let mut f = text.trim().split('|');
    let attached = f.next().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0) > 0;
    let term = f.next().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let w = f.next().and_then(|s| s.trim().parse::<u16>().ok());
    let h = f.next().and_then(|s| s.trim().parse::<u16>().ok());
    let cell = match (w, h) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    };
    TmuxClient { attached, term, cell }
}

/// Pixel size of one cell. Inside tmux the pane pty reports no pixel size, but
/// the tmux server knows the attached client's cell size exactly (see
/// [`tmux_client`]) — using it keeps the rendered canvas aligned to real cell
/// boundaries (a wrong guess
/// leaves sub-cell slivers at the image's right/bottom edges that terminals
/// fill with the sixel background, i.e. white strips). Outside tmux, TIOCGWINSZ;
/// falls back to a common 8×16.
fn cell_pixel_size() -> (u16, u16) {
    if in_tmux() {
        if let Some(wh) = tmux_client().cell {
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

    /// Spawn a renderer that outlives a naive kill: a shell that does *not*
    /// `exec`, so the `sleep` is a grandchild holding the stdout pipe open. That
    /// is the shape a `[preview]` wrapper script has, and waiting for EOF on a
    /// pipe a grandchild still holds is a hang all of its own.
    fn stubborn_renderer() -> Child {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30; echo done"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
        cmd.spawn().expect("spawn wrapper")
    }

    /// A renderer we've stopped caring about must actually die. Without this the
    /// child outlives rustranger — reparented to init, still rasterizing a page
    /// nobody will see.
    /// The forced-exit path must actually kill a running renderer: spawn a real
    /// child in its own process group (as `run_renderer` does), register it, and
    /// check `kill_live_renderers` takes it down.
    #[test]
    fn force_exit_kills_a_registered_renderer() {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        // SAFETY: setpgid is async-signal-safe and is all the closure does.
        unsafe { cmd.pre_exec(|| { libc::setpgid(0, 0); Ok(()) }) };
        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id();

        let registered = Registered::new(pid);
        assert!(LIVE_RENDERERS.lock().unwrap().contains(&pid), "tracked while running");
        // Still alive before we ask it to die.
        assert!(matches!(child.try_wait(), Ok(None)), "sleep should still be running");

        kill_live_renderers();
        let status = child.wait().expect("wait");
        assert!(!status.success(), "killed, not a clean exit: {status:?}");

        // Deregistering on return is what keeps the kill list from growing or
        // naming a pid the OS has since handed to somebody else.
        drop(registered);
        assert!(!LIVE_RENDERERS.lock().unwrap().contains(&pid), "untracked once done");
    }

    #[test]
    fn cancelling_kills_the_renderer() {
        let mut child = stubborn_renderer();
        let group = child.id() as libc::pid_t;
        let cancel = AtomicBool::new(true); // already abandoned
        let started = Instant::now();
        let (_, _, outcome) = collect(&mut child, &cancel, Duration::from_secs(60)).unwrap();
        assert!(matches!(outcome, Outcome::Cancelled), "should report a cancel");
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
        // Nothing of that renderer is left running, grandchildren included.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            unsafe { libc::kill(-group, 0) },
            -1,
            "the renderer's process group is still alive"
        );
    }

    /// A renderer that never finishes must not be waited on forever: that is what
    /// pins the run loop to its 50 ms "a preview is coming" tick for good.
    #[test]
    fn a_stuck_renderer_times_out_and_is_killed() {
        let mut child = stubborn_renderer();
        let started = Instant::now();
        let (_, _, outcome) =
            collect(&mut child, &AtomicBool::new(false), Duration::from_millis(200)).unwrap();
        assert!(matches!(outcome, Outcome::TimedOut), "should report a timeout");
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    /// The whole reason both pipes are drained as they fill: a full-page sixel is
    /// far larger than a pipe buffer, and a renderer nobody reads simply blocks.
    #[test]
    fn collects_far_more_output_than_a_pipe_buffer_holds() {
        const BYTES: usize = 1_000_000;
        let mut child = Command::new("/bin/sh")
            .args(["-c", &format!("yes x | head -c {BYTES}")])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn writer");
        let (out, _, outcome) =
            collect(&mut child, &AtomicBool::new(false), Duration::from_secs(30)).unwrap();
        assert!(matches!(outcome, Outcome::Exited(s) if s.success()), "writer should finish");
        assert_eq!(out.len(), BYTES, "every byte of the render must reach us");
    }

    /// The reply shapes that actually come back from tmux 3.4: nobody attached,
    /// a client attached, and a server too old to know the cell-size formats.
    #[test]
    fn tmux_reply_parsing_covers_attached_detached_and_old_servers() {
        let detached = parse_tmux_client("0|||");
        assert!(!detached.attached, "nobody is watching");
        assert!(detached.term.is_none() && detached.cell.is_none());

        let attached = parse_tmux_client("1|xterm-ghostty|9|18");
        assert!(attached.attached);
        assert_eq!(attached.term.as_deref(), Some("xterm-ghostty"));
        assert_eq!(attached.cell, Some((9, 18)));

        // An old server expands unknown formats to nothing: still attached, and
        // still worth rendering — we just fall back for the cell size.
        let old = parse_tmux_client("1|xterm-256color||");
        assert!(old.attached);
        assert_eq!(old.term.as_deref(), Some("xterm-256color"));
        assert_eq!(old.cell, None);

        // A garbled or empty answer must read as "no client", never as attached.
        assert!(!parse_tmux_client("").attached);
        assert!(!parse_tmux_client("nonsense").attached);
    }

    /// The UI thread runs this, so a wedged tmux server must cost a hitch, not
    /// the session.
    #[test]
    fn a_hung_helper_is_bounded_and_killed() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30"]);
        let started = Instant::now();
        assert!(run_bounded(cmd, Duration::from_millis(200)).is_none(), "should give up");
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
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
