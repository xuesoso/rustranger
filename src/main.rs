use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};

// Modules live in the library crate (src/lib.rs); the binary just drives them.
use rustranger::app::{self, App};
use rustranger::input::{Input, Parser};
use rustranger::{appearance, config, fs, open, tty, ui};

/// Internal pending-key sentinel for the `um{key}` chord (awaiting the bookmark
/// key to delete). Not a typeable key, so it never collides with a real prefix.
const UNBOOKMARK_PENDING: char = '\u{1}';

/// Set by the SIGTERM/SIGHUP handler. The run loop checks it every tick (the
/// idle poll is 500 ms), so a plain `kill` or a closed terminal exits through
/// the normal path — restoring raw mode and the alternate screen — instead of
/// leaving the shell on a raw, drawn-over screen.
static TERMINATE: AtomicBool = AtomicBool::new(false);

/// How long the hangup watchdog lets the run loop shut down cleanly before it
/// exits the process itself. Generous next to the 500 ms idle poll, but far
/// short of "burns a core until the user notices".
const HANGUP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// True while a blocking external program (editor/pager/shell) owns the terminal
/// and rustranger is merely the parent waiting behind it. The shutdown backstop
/// stands down for the duration: that wait is legitimate, however long it runs.
static SUSPENDED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_sig: libc::c_int) {
    // A relaxed atomic store is async-signal-safe; everything else happens on
    // the main thread once the loop observes the flag.
    TERMINATE.store(true, Ordering::Relaxed);
}

struct Args {
    start: PathBuf,
    /// File-picker mode: write the chosen file's path here and exit.
    choosefile: Option<PathBuf>,
    /// Settings overrides from the command line, applied on top of the config file.
    overrides: Vec<(String, String)>,
}

const HELP: &str = "\
usage: rustranger [PATH] [options]
       rustranger gen-config     write a default config.toml (only if missing)

  --choosefile FILE         file-picker mode: write the chosen path and exit
  --theme NAME              auto|default|gruvbox-dark|gruvbox-light|solarized-dark|
                            solarized-light|nord|dracula|subliminal|gitlab-dark|
                            gitlab-light|everforest-dark|everforest-light|
                            one-light|ayu-light  (auto: light_theme/dark_theme
                            to match the terminal background, switching live)
  --sort KEY                natural|basename|size|mtime|ctime|atime|type|extension|random
  --reverse                 reverse the sort order
  --show-date               show the date column
  --no-date                 hide the date column
  --time-type TYPE          modified|created|changed|accessed
  --time-format FMT         date (YYYY/MM/DD) | datetime (YYYY/MM/DD/HH/MM)
  --size-format FMT         human|binary|bytes
  --set KEY=VALUE           override any config.toml setting (repeatable)
  -v, -V, --version         print version and exit
  -h, --help                show this help

Persistent defaults live in ~/.config/rustranger/config.toml; the flags above
override them for this run.";

fn parse_args() -> Args {
    let mut start: Option<PathBuf> = None;
    let mut choosefile: Option<PathBuf> = None;
    let mut overrides: Vec<(String, String)> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(raw) = it.next() {
        // Support both "--flag value" and "--flag=value" forms.
        let (flag, inline_val) = match raw.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (raw.clone(), None),
        };
        let mut take_val = || inline_val.clone().or_else(|| it.next());
        match flag.as_str() {
            "--choosefile" | "--selectfile" => choosefile = take_val().map(PathBuf::from),
            "-h" | "--help" => {
                println!("{}", HELP);
                std::process::exit(0);
            }
            "-v" | "-V" | "--version" => {
                println!("rustranger {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--theme" => {
                if let Some(v) = take_val() {
                    overrides.push(("theme".into(), v));
                }
            }
            "--sort" => {
                if let Some(v) = take_val() {
                    overrides.push(("sort".into(), v));
                }
            }
            "--reverse" => overrides.push(("sort_reverse".into(), "true".into())),
            "--show-date" | "--show-time" => overrides.push(("show_date".into(), "true".into())),
            "--no-date" | "--no-time" | "--hide-date" => {
                overrides.push(("show_date".into(), "false".into()))
            }
            "--time-type" => {
                if let Some(v) = take_val() {
                    overrides.push(("time_type".into(), v));
                }
            }
            "--time-format" => {
                if let Some(v) = take_val() {
                    overrides.push(("time_format".into(), v));
                }
            }
            "--size-format" => {
                if let Some(v) = take_val() {
                    overrides.push(("size_format".into(), v));
                }
            }
            "--set" | "-s" => {
                if let Some((k, v)) = take_val().and_then(|kv| {
                    kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))
                }) {
                    overrides.push((k, v));
                }
            }
            s if !s.starts_with('-') && start.is_none() => start = Some(PathBuf::from(raw)),
            _ => {}
        }
    }
    let start = start.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    Args {
        start: start.canonicalize().unwrap_or(start),
        choosefile,
        overrides,
    }
}

fn main() -> io::Result<()> {
    // Subcommands are recognized only as the first argument.
    if std::env::args().nth(1).as_deref() == Some("gen-config") {
        return gen_config();
    }

    let args = parse_args();
    // Config file defaults, then command-line overrides on top.
    let mut settings = config::Settings::load();
    for (key, value) in &args.overrides {
        settings.set_field(key, value);
    }
    let mut app = App::new(args.start, settings);
    app.choosefile = args.choosefile;

    // Exit cleanly on SIGTERM/SIGHUP (`kill`, terminal window closed).
    let handler = on_terminate as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }

    setup_terminal()?;
    // The terminal can vanish under us: window closed, emulator quit, ssh
    // dropped. The SIGHUP that comes with it is caught above rather than fatal,
    // so noticing the hangup is on us. Watch for it directly; the run loop then
    // leaves through the normal path, and the watchdog force-exits if it can't
    // (see `tty`), so we never leave a detached CPU-spinning orphan.
    tty::watch_hangup(
        HANGUP_GRACE,
        || TERMINATE.store(true, Ordering::Relaxed),
        // Reached only if the run loop never got to shut down. `_exit` runs no
        // destructors, so the renderer's own cleanup will not happen — kill it
        // here or it outlives us as an orphan.
        rustranger::imgpreview::kill_live_renderers,
    );
    // ...and a backstop for a shutdown that starts but never finishes.
    watch_shutdown(HANGUP_GRACE);
    // Restore the terminal even if we panic mid-render.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        prev_hook(info);
    }));

    let result = run(&mut app);

    restore_terminal()?;
    result
}

/// Force-exit backstop for a shutdown that never finishes.
///
/// Setting `TERMINATE` *asks* the run loop to leave, and normally it does so
/// within one idle tick. But the main thread can be somewhere it will not come
/// back from by itself — most reachably blocked in `write(2)` to a terminal that
/// has stopped draining (flow control, a frozen or suspended emulator). Signals
/// installed with `signal(3)` restart such a call rather than failing it, and
/// `write_all` retries `EINTR` regardless, so the loop never regains control to
/// notice the flag. The process then sits there having been told to die: a
/// dangling process that plain `kill` cannot shift.
///
/// So once asked to terminate, give the loop `grace`, then take any renderers
/// with us and go. Held off while an external program owns the terminal.
fn watch_shutdown(grace: std::time::Duration) {
    std::thread::spawn(move || {
        let tick = std::time::Duration::from_millis(100);
        loop {
            std::thread::sleep(tick);
            if !TERMINATE.load(Ordering::Relaxed) {
                continue;
            }
            std::thread::sleep(grace);
            // An external program is running: rustranger is supposed to be
            // waiting, so this is not a stuck shutdown. Re-check later.
            if SUSPENDED.load(Ordering::Relaxed) {
                continue;
            }
            // `_exit` runs no destructors, so clean up what would have been.
            rustranger::imgpreview::kill_live_renderers();
            // SAFETY: _exit is async-signal-safe and performs no unwinding.
            unsafe { libc::_exit(0) };
        }
    });
}

/// `gen-config` subcommand: write a default config.toml, but only if one does not
/// already exist (never overwrites).
fn gen_config() -> io::Result<()> {
    match config::generate_default_config()? {
        config::GenConfig::Created(p) => println!("Wrote default config to {}", p.display()),
        config::GenConfig::Exists(p) => {
            println!("Config already exists at {} (left unchanged).", p.display())
        }
        config::GenConfig::NoConfigDir => {
            eprintln!("Could not determine the config directory (set $HOME or $XDG_CONFIG_HOME).");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn setup_terminal() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // DisableLineWrap: the diff renderer may write the last cell of a row, and
    // autowrap there would scroll/shift the screen.
    execute!(out, EnterAlternateScreen, cursor::Hide, crossterm::terminal::DisableLineWrap)?;
    Ok(())
}

fn restore_terminal() -> io::Result<()> {
    let mut out = io::stdout();
    // Stop mouse reporting first: leaving it on would make the shell we return to
    // (or a panic'd exit) spew escape sequences on every click.
    out.write_all(MOUSE_OFF.as_bytes())?;
    // Likewise scheme-change notifications (a no-op if never subscribed).
    out.write_all(appearance::NOTIFY_OFF)?;
    execute!(out, crossterm::terminal::EnableLineWrap, cursor::Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

/// Mouse reporting: button press/release (mode 1000) with SGR extended coordinates
/// (mode 1006, which lifts the 223-column limit so clicks land correctly on wide
/// terminals).
///
/// Deliberately *not* crossterm's `EnableMouseCapture`, which also turns on
/// any-event tracking (mode 1003): that reports every pointer *motion*, waking the
/// run loop — and its per-frame directory re-stat — for events we never use.
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1000l";

fn set_mouse(out: &mut impl Write, on: bool) -> io::Result<()> {
    out.write_all(if on { MOUSE_ON } else { MOUSE_OFF }.as_bytes())?;
    out.flush()
}

fn run(app: &mut App) -> io::Result<()> {
    use rustranger::screen::{self, Buffer, Style};
    let mut out = io::stdout();
    let mut pending: Option<char> = None;
    let mut count: Option<usize> = None;
    // Double-buffered cell grid: render into `cur`, emit only the cells that
    // differ from the previously displayed `prev`, then swap. This keeps each
    // frame's output tiny (a cursor move repaints a couple of rows, not the
    // whole screen), which is what stops the flicker over tmux/SSH.
    let mut frame: Vec<u8> = Vec::with_capacity(32 * 1024);
    let default = Style::new(crossterm::style::Color::Reset, crossterm::style::Color::Reset);
    let mut cur = Buffer::new(0, 0, default);
    let mut prev: Option<Buffer> = None;
    // Forces a full repaint (clear + no diff baseline) on the next frame. Set on
    // resize and after a blocking external program (editor/pager/shell), whose
    // output left the real screen contents unknown to our diff model.
    let mut needs_full = false;
    // In-pane image/document preview (kitty/sixel graphics over the preview box).
    let mut img = rustranger::imgpreview::ImagePreview::new();
    // Detached programs we started (`[open]` entries ending in `&`). They outlive
    // the launch by design, so they are reaped here rather than waited on.
    let mut detached: Vec<std::process::Child> = Vec::new();
    // Whether the terminal is currently reporting mouse events. Re-synced against
    // the setting every frame, so `zm` takes effect immediately and an external
    // program that clobbered the mode gets it restored.
    let mut mouse_on = false;
    // Cursor state as the terminal last saw it, so an unchanged frame can skip
    // its write. `None` until the first frame, which therefore always writes.
    let mut last_cursor: Option<Option<(u16, u16)>> = None;
    // Window resizes wake our own wait (see `tty::install_resize_wake`).
    tty::install_resize_wake();
    // Terminal input, decoded by us rather than crossterm so the terminal's
    // replies (background color, scheme changes) can be told from keystrokes.
    let mut input = Parser::new();
    let mut read_buf = [0u8; 1024];
    // `theme = "auto"`: when to ask the terminal for its background again.
    let mut look = appearance::Tracker::default();
    // Ask before the first paint, so it is drawn in the right theme.
    if app.settings.theme_auto() {
        query_background(&mut out, &mut look)?;
        if !settle(&mut input, &mut read_buf, &mut look, appearance::STARTUP_WAIT) {
            return Ok(());
        }
        let bg = input.pending().filter_map(|i| match *i {
            Input::Background(r, g, b) => Some((r, g, b)),
            _ => None,
        });
        if let Some((r, g, b)) = bg.last() {
            app.settings.set_dark_background(appearance::rgb_is_dark(r, g, b));
        }
    }
    // The background the screen was last painted with: a theme switch (auto, or
    // `:set theme`) repaints everything — and re-renders the image preview,
    // whose fill and colors follow the background.
    let mut painted_bg = app.settings.theme.bg;
    while !app.quit && !TERMINATE.load(Ordering::Relaxed) {
        if mouse_on != app.settings.mouse_enabled {
            set_mouse(&mut out, app.settings.mouse_enabled)?;
            mouse_on = app.settings.mouse_enabled;
        }
        // Subscribe to scheme-change notifications while following the terminal
        // (also after an external program, which unsubscribes) — and look again
        // now, as the theme may have changed while we weren't listening.
        let auto = app.settings.theme_auto();
        if look.subscribed != auto {
            out.write_all(if auto { appearance::NOTIFY_ON } else { appearance::NOTIFY_OFF })?;
            look.subscribed = auto;
            if auto {
                query_background(&mut out, &mut look)?;
            }
            out.flush()?;
        }
        if app.settings.theme.bg != painted_bg {
            painted_bg = app.settings.theme.bg;
            needs_full = true;
        }
        app.prepare_view();
        // Our own ioctl on the fd we already hold; crossterm's `size()` reopens
        // /dev/tty every call. Falls back to it when there is no terminal fd.
        let size = tty::size().or_else(|| crossterm::terminal::size().ok());
        if let Some((cols, rows)) = size {
            let (cols, rows) = (cols as usize, rows as usize);
            if cur.cols != cols || cur.rows != rows {
                cur = Buffer::new(cols, rows, default);
                needs_full = true;
            }
            // On first paint / resize / resume-from-editor, drop the diff baseline
            // and clear once so the whole screen is repainted (not just changed
            // cells against a now-stale model).
            if needs_full {
                prev = None;
                frame.clear();
                // Clear to the active theme's background (not the terminal default)
                // so first paint / resize / resume-from-editor never flashes the
                // terminal's bare background before the themed frame lands on top.
                let clear_style = Style::new(app.settings.theme.fg, app.settings.theme.bg);
                screen::clear(&mut frame, clear_style)?;
                out.write_all(&frame)?;
                needs_full = false;
                // The real screen was wiped (resize / resume from a full-screen
                // child), which also drops any graphics image; forget it so the
                // sync below re-emits it fresh at the new geometry.
                img.forget();
            }
            let cursor = ui::render(&mut cur, app);
            frame.clear();
            let cells_changed = screen::flush(&mut frame, prev.as_ref(), &cur)?;
            // Cursor: shown at the console edit position, hidden otherwise.
            match cursor {
                Some((cx, cy)) => {
                    crossterm::queue!(frame, crossterm::cursor::MoveTo(cx, cy), crossterm::cursor::Show)?
                }
                None => crossterm::queue!(frame, crossterm::cursor::Hide)?,
            }
            // An idle tick usually changes nothing. Writing the frame anyway
            // means a syscall and a synchronized-update wrapper down the wire
            // twice a second forever — noise on an ssh or tmux link, and a
            // wake-up for the terminal, to redraw a screen that is identical.
            if cells_changed || last_cursor != Some(cursor) {
                out.write_all(&frame)?;
                out.flush()?;
                last_cursor = Some(cursor);
            }
            // Paint (or clear) the in-pane image preview over the just-flushed
            // cells. A sixel clear needs its pixels painted over, so it asks for a
            // full repaint next frame.
            let sync = img.sync(&mut out, app, cols, rows)?;
            if sync.needs_full {
                needs_full = true;
            }
            // `clear` blanked this preview-box rect directly on-screen, outside the
            // cell diff. Blank the same cells in `cur` (which becomes the next diff
            // baseline) so the following frame repaints whatever now belongs there —
            // notably a key-chain menu popup overlapping the box, which would else
            // stay hidden under the image we just erased.
            if let Some((bx, btop, bw, bh)) = sync.cleared {
                let blank = Style::new(app.settings.theme.fg, app.settings.theme.bg);
                let spaces = " ".repeat(bw as usize);
                for r in 0..bh {
                    cur.set_str(bx as usize, (btop + r) as usize, &spaces, blank);
                }
            }
            // Snapshot the displayed frame as the next diff baseline. Swapping
            // buffers (instead of cloning) avoids a fresh allocation and a full
            // grid copy per frame; render() rebuilds `cur` from scratch anyway,
            // and `prev` is dropped to None whenever the size changes.
            match prev.as_mut() {
                Some(p) => std::mem::swap(p, &mut cur),
                None => prev = Some(cur.clone()),
            }
        }

        // Wait for input: poll briefly when background jobs run (so progress
        // updates); otherwise poll on a slow idle tick so changes made to the
        // visible directories by other programs show up without a keypress
        // (prepare_view re-checks their mtimes — see App::refresh_from_disk).
        // An idle no-change tick costs 2-3 stats and a diff that emits nothing.
        let idle_ms = if app.jobs_active() {
            80
        } else if img.wants_tick() {
            // A preview is waiting out its debounce — wake soon to render it.
            50
        } else {
            500
        };
        // Collect any detached program that has since exited, so a long session
        // of opening files doesn't accumulate zombies.
        reap(&mut detached);
        // Input already decoded (left over when a key launched a program) comes
        // first; otherwise sleep on the fd until something arrives.
        if !input.has_pending() {
            match tty::wait(std::time::Duration::from_millis(idle_ms)) {
                // The terminal is gone: leave through the normal shutdown path.
                tty::Wait::Hangup => break,
                tty::Wait::Ready => {
                    if !fill(&mut input, &mut read_buf) {
                        break;
                    }
                }
                // A resize just needs the loop to turn: it re-reads the size and
                // repaints at the top. A timeout is the idle tick.
                tty::Wait::Resized | tty::Wait::Timeout => {}
            }
        }
        // Coalesce a burst: handle everything already decoded — and whatever more
        // is already waiting on the fd — before the next redraw, so fast
        // key-repeat (which backs up behind a slow tmux/SSH redraw) collapses into
        // ONE frame instead of one frame per key. This self-paces rendering to
        // whatever the display can keep up with.
        let size = (cur.cols, cur.rows);
        let mut pressed = false;
        let mut rescheme = false;
        let mut hung_up = false;
        // (Skipped on an idle tick: nothing arrived, so there is nothing to drain.)
        'burst: while input.has_pending() {
            while let Some(next) = input.next() {
                match next {
                    Input::Event(ev) => {
                        pressed |= matches!(ev, Event::Key(_) | Event::Mouse(_));
                        handle_event(app, ev, &mut pending, &mut count, size);
                    }
                    Input::Background(r, g, b) => {
                        app.settings.set_dark_background(appearance::rgb_is_dark(r, g, b));
                    }
                    Input::SchemeChanged(_) => rescheme = true,
                    Input::DeviceAttributes => look.answered(),
                    Input::Reply => {}
                }
                // Don't keep swallowing keys once a key has asked to launch an
                // external program or quit (those keys belong to the next state;
                // they stay queued in `input`).
                if app.pending_run.is_some() || app.quit {
                    break 'burst;
                }
            }
            match tty::wait(std::time::Duration::ZERO) {
                tty::Wait::Ready if fill(&mut input, &mut read_buf) => {}
                tty::Wait::Ready | tty::Wait::Hangup => {
                    hung_up = true;
                    break;
                }
                tty::Wait::Resized | tty::Wait::Timeout => break,
            }
        }
        if hung_up {
            break;
        }
        // `theme = "auto"`: look at the background again — at once when the
        // terminal says its scheme changed, else on a key press (paced). Never
        // just before handing the terminal to a program or the shell, which
        // would receive the answer as typed text.
        if app.settings.theme_auto() && app.pending_run.is_none() && !app.quit {
            let now = std::time::Instant::now();
            if (rescheme && look.notification_due(now)) || (pressed && look.key_due(now)) {
                query_background(&mut out, &mut look)?;
            }
        }
        if app.jobs_active() {
            app.tick_jobs();
        }

        // Run any external program requested this iteration. A blocking program
        // (editor/pager/shell) suspends the TUI, so force a full repaint after.
        if let Some(req) = app.pending_run.take() {
            // A query answer still on its way would reach the child as typed
            // text: wait for it (bounded), and stop scheme notifications.
            if !settle(&mut input, &mut read_buf, &mut look, appearance::SETTLE_WAIT) {
                break;
            }
            if look.subscribed {
                let _ = out.write_all(appearance::NOTIFY_OFF);
                look.subscribed = false;
            }
            // Remove any preview image so it doesn't linger over the child program.
            let _ = img.clear(&mut out);
            // Hand the mouse back to the child (and to the terminal's own
            // selection). Clearing the flag re-arms the sync at the top of the
            // loop, which restores our mode once the child is gone.
            if mouse_on {
                let _ = set_mouse(&mut out, false);
                mouse_on = false;
            }
            let bg = app.settings.theme.bg;
            let override_bg = app.settings.override_terminal_bg;
            let (suspended, error) =
                run_external(&mut out, req, bg, override_bg, &mut detached)?;
            if suspended {
                needs_full = true;
            }
            // Surface a launch failure (e.g. a mistyped $EDITOR) in the status
            // bar instead of flashing the screen and saying nothing.
            if error.is_some() {
                app.message = error;
            }
        }
    }
    // Remove any lingering preview image before leaving the alternate screen.
    let _ = img.clear(&mut out);
    out.flush()?;
    // Don't leave a query answer behind for the shell to print at its prompt.
    settle(&mut input, &mut read_buf, &mut look, appearance::SETTLE_WAIT);
    Ok(())
}

/// Ask the terminal for its background color, followed by `DA1` so we can tell
/// when it has finished answering. Flushes, so the question goes out now.
fn query_background(out: &mut impl Write, look: &mut appearance::Tracker) -> io::Result<()> {
    out.write_all(appearance::QUERY_BACKGROUND)?;
    out.write_all(appearance::QUERY_DEVICE_ATTRIBUTES)?;
    look.sent(std::time::Instant::now());
    out.flush()
}

/// Read whatever the terminal sent into the decoder. `false` on hangup.
fn fill(input: &mut Parser, buf: &mut [u8]) -> bool {
    match tty::read(buf) {
        // A full buffer means more is likely waiting (crossterm's heuristic for
        // whether a trailing ESC begins a sequence).
        tty::Read::Bytes(n) => {
            input.advance(&buf[..n], n == buf.len());
            true
        }
        tty::Read::Nothing => true,
        tty::Read::Hangup => false,
    }
}

/// Wait, up to `limit`, for the answer to an in-flight background query to
/// arrive in full (its `DA1`). Everything read meanwhile stays queued in
/// `input`, in order, for the run loop. Returns `false` if the terminal hung up.
fn settle(input: &mut Parser, buf: &mut [u8], look: &mut appearance::Tracker, limit: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let now = std::time::Instant::now();
        if input.pending().any(|i| *i == Input::DeviceAttributes) || !look.in_flight(now) || now >= deadline {
            return true;
        }
        match tty::wait(deadline - now) {
            tty::Wait::Ready => {
                if !fill(input, buf) {
                    return false;
                }
            }
            tty::Wait::Hangup => return false,
            tty::Wait::Resized | tty::Wait::Timeout => {}
        }
    }
}

/// Wait for any detached child that has exited, dropping it from the list.
///
/// A `std::process::Child` neither kills nor waits on drop, so without this every
/// `[open]` program run with a trailing `&` stays in the process table as a zombie
/// until rustranger itself exits. Children still running are kept and re-checked
/// next tick; they are meant to outlive us, and are left alone at quit.
fn reap(detached: &mut Vec<std::process::Child>) {
    detached.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
}

/// Dispatch a single terminal event (ignores key releases and unhandled events;
/// resize is handled implicitly since the loop re-queries the size each frame).
/// `size` is the last rendered `(cols, rows)`, needed to hit-test mouse clicks.
fn handle_event(
    app: &mut App,
    ev: Event,
    pending: &mut Option<char>,
    count: &mut Option<usize>,
    size: (usize, usize),
) {
    match ev {
        Event::Key(key) if key.kind != KeyEventKind::Release => handle_key(app, key, pending, count),
        Event::Mouse(me) => handle_mouse(app, me, pending, count, size),
        _ => {}
    }
}

/// Rows the wheel moves per notch — the usual desktop convention, and the same
/// step `J`/`K` use for the preview pane.
const WHEEL_ROWS: isize = 3;

/// Dispatch a mouse event: the wheel scrolls, left click selects-and-enters (like
/// `l`), right click goes to the parent (like `h`). Only ever called while mouse
/// reporting is on, since the terminal reports nothing otherwise.
fn handle_mouse(
    app: &mut App,
    me: crossterm::event::MouseEvent,
    pending: &mut Option<char>,
    count: &mut Option<usize>,
    (cols, rows): (usize, usize),
) {
    use crossterm::event::{MouseButton, MouseEventKind};

    // A y/n confirmation and the console own every input: a stray click must never
    // answer a delete prompt or disturb a half-typed command.
    if app.confirm.is_some() || app.console.is_some() {
        return;
    }

    // The help overlay scrolls with the wheel; clicks do nothing.
    if let Some(scroll) = app.help {
        let step = WHEEL_ROWS as usize;
        match me.kind {
            MouseEventKind::ScrollUp => app.help = Some(scroll.saturating_sub(step)),
            MouseEventKind::ScrollDown => {
                app.help = Some((scroll + step).min(ui::help_len().saturating_sub(1)))
            }
            _ => {}
        }
        return;
    }

    // A key-chain menu is a hint awaiting its second key. A click dismisses it
    // (like Esc) and is otherwise swallowed — clearing the pending prefix too, so
    // it can't silently consume the next keystroke.
    if app.menu.is_some() {
        if matches!(me.kind, MouseEventKind::Down(_)) {
            app.menu = None;
            *pending = None;
            *count = None;
        }
        return;
    }

    match me.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let delta = if matches!(me.kind, MouseEventKind::ScrollDown) {
                WHEEL_ROWS
            } else {
                -WHEEL_ROWS
            };
            // Over the preview pane the wheel scrolls the preview (like J/K);
            // anywhere else it moves the cursor through the listing.
            if matches!(ui::hit_test(app, cols, rows, me.column, me.row), ui::Hit::Preview) {
                app.scroll_preview(delta);
            } else {
                app.move_cursor(delta);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            app.message = None;
            match ui::hit_test(app, cols, rows, me.column, me.row) {
                // Select what was clicked, then act on it — enter a directory or
                // open a file, exactly as `l` / Enter would.
                ui::Hit::Current(i) => {
                    app.select_index(i);
                    app.enter();
                }
                // In the parent column a click means "go to that directory".
                ui::Hit::Parent(i) => app.enter_parent_entry(i),
                ui::Hit::Preview | ui::Hit::Nothing => {}
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            app.message = None;
            app.ascend();
        }
        _ => {} // middle click, button release, motion: nothing to do
    }
    *count = None; // a click ends any numeric prefix
}

/// Run an external program. Blocking programs (editors/pager/shell) suspend the
/// TUI; forked GUI programs are detached and the TUI keeps running.
///
/// Returns `(suspended, error)`: `suspended` is true when the TUI was suspended
/// (a blocking program ran), so the caller knows the terminal contents are now
/// unknown and the next frame must be a full repaint rather than a diff against
/// the pre-launch screen. `error` carries a launch failure (e.g. a mistyped
/// `$EDITOR`) for the status bar; a program that ran and exited nonzero is its
/// own business and is not reported.
fn run_external(
    out: &mut io::Stdout,
    req: open::RunRequest,
    bg: crossterm::style::Color,
    override_bg: bool,
    detached: &mut Vec<std::process::Child>,
) -> io::Result<(bool, Option<String>)> {
    use std::process::{Command, Stdio};
    if req.argv.is_empty() {
        return Ok((false, None));
    }
    let mut cmd = Command::new(&req.argv[0]);
    cmd.args(&req.argv[1..]).current_dir(&req.cwd);

    if req.block {
        SUSPENDED.store(true, Ordering::Relaxed);
        if req.fullscreen {
            suspend_keep_screen(bg, override_bg)?;
        } else {
            restore_terminal()?;
        }
        let status = cmd.status();
        if req.fullscreen {
            resume_keep_screen(bg, override_bg)?;
        } else {
            setup_terminal()?;
        }
        out.flush()?;
        SUSPENDED.store(false, Ordering::Relaxed);
        let error = status
            .err()
            .map(|e| format!("failed to run {}: {}", req.argv[0], e));
        Ok((true, error))
    } else {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Keep the handle: a `Child` that is simply dropped is never waited for,
        // so every GUI file we opened would leave a zombie sitting in the process
        // table for as long as rustranger runs. The loop reaps these each tick.
        let error = match cmd.spawn() {
            Ok(child) => {
                detached.push(child);
                None
            }
            Err(e) => Some(format!("failed to run {}: {}", req.argv[0], e)),
        };
        Ok((false, error))
    }
}

/// Suspend the TUI for a full-screen child *without leaving the alternate screen*.
/// A full-screen program (editor/pager/TUI) enters its own alternate screen and
/// clears it to its own background, so keeping ours active means the terminal never
/// drops to the primary buffer's default background — eliminating the dark→white→
/// dark flash and leaving the shell's scrollback untouched.
///
/// The remaining window is *before* the child's own theme loads: a captured nvim
/// startup shows it reset SGR (`ESC[0m`) and repaint every row with the terminal's
/// **default** background, so our SGR background alone doesn't cover it. Overriding
/// the terminal's *default* background with `OSC 11` for the duration of the child
/// does — but only by lying to the child about the terminal's colors, which breaks
/// programs that pick a light/dark theme by querying `OSC 11` (nvim, and anything
/// doing automatic appearance switching: our answer fights the real system state).
/// So that override is opt-in via `override_terminal_bg`, off by default: a brief
/// flash beats hijacking the editor's theme. (No-op either way for the `default`
/// theme, whose background is the terminal default already.)
fn suspend_keep_screen(bg: crossterm::style::Color, override_bg: bool) -> io::Result<()> {
    let mut out = io::stdout();
    out.write_all(&suspend_bytes(bg, override_bg))?;
    out.flush()?;
    disable_raw_mode()?;
    Ok(())
}

/// The escape sequence [`suspend_keep_screen`] emits (split out to be testable):
/// show the cursor, restore wrapping, paint our theme background over the screen,
/// and — only with `override_bg` — lie about the terminal's default background.
fn suspend_bytes(bg: crossterm::style::Color, override_bg: bool) -> Vec<u8> {
    use crossterm::style::SetBackgroundColor;
    use crossterm::terminal::{Clear, ClearType, EnableLineWrap};
    let mut seq: Vec<u8> = Vec::new();
    let _ = crossterm::queue!(seq, cursor::Show, EnableLineWrap, SetBackgroundColor(bg), Clear(ClearType::All));
    if override_bg {
        if let Some(osc) = osc_set_default_bg(bg) {
            seq.extend_from_slice(osc.as_bytes());
        }
    }
    seq
}

/// Resume after a full-screen child. Undo the `OSC 11` default-background override
/// if we set one, then re-assert our alternate screen — with the theme background
/// set first so the enter-clear is dark, not the terminal default. A full repaint
/// follows on the next frame (the caller returns `true`).
fn resume_keep_screen(bg: crossterm::style::Color, override_bg: bool) -> io::Result<()> {
    let mut out = io::stdout();
    enable_raw_mode()?;
    out.write_all(&resume_bytes(bg, override_bg))?;
    out.flush()?;
    Ok(())
}

/// The escape sequence [`resume_keep_screen`] emits (split out to be testable).
fn resume_bytes(bg: crossterm::style::Color, override_bg: bool) -> Vec<u8> {
    use crossterm::style::SetBackgroundColor;
    use crossterm::terminal::DisableLineWrap;
    let mut seq: Vec<u8> = Vec::new();
    // Reset only what we actually set: an unconditional OSC 111 would clobber a
    // default background someone else (a shell theme script) had configured.
    if override_bg && osc_set_default_bg(bg).is_some() {
        seq.extend_from_slice(OSC_RESET_DEFAULT_BG.as_bytes());
    }
    let _ = crossterm::queue!(seq, SetBackgroundColor(bg), EnterAlternateScreen, cursor::Hide, DisableLineWrap);
    seq
}

/// `OSC 11` sequence setting the terminal's *default* background to a concrete RGB
/// theme colour (`ESC ] 11 ; rgb:RR/GG/BB BEL`). Returns `None` for `Color::Reset`
/// (the `default` theme) and non-RGB colours, where there is nothing to override.
/// Handled natively by tmux 3.3+ for the pane, so it works inside tmux and SSH.
fn osc_set_default_bg(bg: crossterm::style::Color) -> Option<String> {
    match bg {
        crossterm::style::Color::Rgb { r, g, b } => {
            Some(format!("\x1b]11;rgb:{:02x}/{:02x}/{:02x}\x07", r, g, b))
        }
        _ => None,
    }
}

/// `OSC 111`: reset the default background to the terminal's own configured value.
const OSC_RESET_DEFAULT_BG: &str = "\x1b]111\x07";

fn handle_key(app: &mut App, key: KeyEvent, pending: &mut Option<char>, count: &mut Option<usize>) {
    use fs::sort::SortKey;

    // The console captures all input while open.
    if app.console.is_some() {
        handle_console_key(app, key);
        return;
    }

    // A pending y/n confirmation captures the next keypress.
    if app.confirm.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => app.answer_confirm(true),
            _ => app.answer_confirm(false),
        }
        return;
    }

    // The help overlay captures input (scroll / close) while open.
    if app.help.is_some() {
        handle_help_key(app, key);
        return;
    }

    // A message (and any open key-chain hint menu) is cleared on the next keypress.
    app.message = None;
    app.menu = None;

    // Numeric prefix: accumulate a repeat count (1-9, then any digit).
    if pending.is_none() {
        if let KeyCode::Char(c) = key.code {
            if c.is_ascii_digit() && !key.modifiers.contains(KeyModifiers::ALT) {
                let d = c.to_digit(10).unwrap() as usize;
                if c != '0' || count.is_some() {
                    // Saturate and cap: an absurdly long digit prefix must not
                    // overflow (debug panic / release wraparound to a negative
                    // isize move) nor stall visual-mode marking.
                    let next = count.unwrap_or(0).saturating_mul(10).saturating_add(d);
                    *count = Some(next.min(1_000_000));
                    return;
                }
            }
        }
    }
    let n = count.take().unwrap_or(1);

    // Resolve any pending multi-key prefix first.
    if let Some(prefix) = pending.take() {
        if let KeyCode::Char(c) = key.code {
            match prefix {
                'g' => match c {
                    'g' => app.move_to_top(),
                    'h' => {
                        if let Some(home) = home_dir() {
                            app.cd(home);
                        }
                    }
                    '/' => app.cd(std::path::PathBuf::from("/")),
                    'n' => app.tab_new(),
                    't' => app.tab_next(),
                    'T' => app.tab_prev(),
                    _ => {}
                },
                // Sort: lowercase sorts ascending, SHIFTed uppercase descending.
                'o' => match c {
                    's' => app.set_sort_order(SortKey::Size, false),
                    'S' => app.set_sort_order(SortKey::Size, true),
                    'n' => app.set_sort_order(SortKey::Natural, false),
                    'N' => app.set_sort_order(SortKey::Natural, true),
                    'b' => app.set_sort_order(SortKey::Basename, false),
                    'B' => app.set_sort_order(SortKey::Basename, true),
                    'm' => app.set_sort_order(SortKey::Mtime, false),
                    'M' => app.set_sort_order(SortKey::Mtime, true),
                    'c' => app.set_sort_order(SortKey::Ctime, false),
                    'C' => app.set_sort_order(SortKey::Ctime, true),
                    'a' => app.set_sort_order(SortKey::Atime, false),
                    'A' => app.set_sort_order(SortKey::Atime, true),
                    't' => app.set_sort_order(SortKey::Type, false),
                    'T' => app.set_sort_order(SortKey::Type, true),
                    'e' => app.set_sort_order(SortKey::Extension, false),
                    'E' => app.set_sort_order(SortKey::Extension, true),
                    'r' => app.toggle_sort_reverse(),
                    'z' => app.set_sort(SortKey::Random),
                    'f' => app.toggle_dirs_first(),
                    _ => {}
                },
                'y' => match c {
                    'y' => app.copy(),
                    'n' => app.yank_name(true),  // name with extension
                    'b' => app.yank_name(false), // base name, no extension
                    'p' => app.yank_path(),
                    _ => {}
                },
                // Toggle settings (ranger's `z` prefix).
                'z' => match c {
                    'h' => app.toggle_hidden(),
                    'i' => app.toggle_preview_images(),
                    'I' => app.toggle_preview_theme(),
                    'm' => app.toggle_mouse(),
                    _ => {}
                },
                'd' => {
                    if c == 'd' {
                        app.cut()
                    }
                }
                'p' => match c {
                    'p' => app.paste(),
                    'l' => app.paste_links(true),
                    'L' => app.paste_links(false),
                    'h' => app.paste_hardlinks(),
                    _ => {}
                },
                'u' => match c {
                    'v' => app.clear_marks(),
                    'm' => {
                        // um{key}: delete a bookmark — await its key, showing the list.
                        *pending = Some(UNBOOKMARK_PENDING);
                        app.menu = Some(app.bookmark_menu("delete bookmark"));
                    }
                    _ => {}
                },
                'm' => app.set_bookmark(c),
                '`' | '\'' => app.enter_bookmark(c),
                p if p == UNBOOKMARK_PENDING => app.delete_bookmark(c),
                'c'
                    if c == 'w' => {
                        // cw: rename, pre-filled with the current name.
                        let name = app
                            .selected_path()
                            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
                            .unwrap_or_default();
                        app.open_console(':', &format!("rename {}", name));
                    }
                _ => {}
            }
        }
        return;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // Alt+digit switches to (or creates) tab N.
    if alt {
        if let KeyCode::Char(c) = key.code {
            if let Some(n) = c.to_digit(10) {
                app.tab_goto(n as usize);
                return;
            }
        }
    }

    match (key.code, ctrl) {
        (KeyCode::Char('q'), false) => app.tab_close(),
        (KeyCode::Char('Q'), false) => app.quit = true,
        (KeyCode::Tab, _) => app.tab_next(),
        (KeyCode::BackTab, _) => app.tab_prev(),
        (KeyCode::Char('H'), false) => app.history_back(),
        (KeyCode::Char('L'), false) => app.history_forward(),
        (KeyCode::Char('t'), false) => app.toggle_tag(),
        (KeyCode::Char('m'), false) => {
            *pending = Some('m');
            app.menu = Some(app.bookmark_menu("set bookmark"));
        }
        (KeyCode::Char('`'), false) | (KeyCode::Char('\''), false) => {
            *pending = Some('`');
            app.menu = Some(app.bookmark_menu("go to bookmark"));
        }
        (KeyCode::Char('j'), false) | (KeyCode::Down, false) => app.move_cursor(n as isize),
        (KeyCode::Char('k'), false) | (KeyCode::Up, false) => app.move_cursor(-(n as isize)),
        (KeyCode::Char('d'), true) => app.move_cursor(10),
        (KeyCode::Char('u'), true) => app.move_cursor(-10),
        (KeyCode::Char('h'), false) | (KeyCode::Left, false) => app.ascend(),
        (KeyCode::Char('l'), false) | (KeyCode::Right, false) | (KeyCode::Enter, _) => app.enter(),
        (KeyCode::Char('G'), false) => app.move_to_bottom(),
        (KeyCode::Char('D'), false) => app.request_delete(),
        (KeyCode::Char('z'), false) => {
            *pending = Some('z');
            app.menu = Some(app::KeyMenu::toggle());
        }
        (KeyCode::Char('J'), false) => app.scroll_preview(3),
        (KeyCode::Char('K'), false) => app.scroll_preview(-3),
        (KeyCode::Char(' '), false) => app.toggle_mark(),
        (KeyCode::Char('v'), false) => app.toggle_visual(),
        (KeyCode::Char('V'), false) => app.toggle_all_marks(),
        (KeyCode::Esc, _) => {
            app.clear_marks();
            *count = None;
        }
        // Console / search.
        (KeyCode::Char(':'), false) => app.open_console(':', ""),
        (KeyCode::Char('/'), false) => app.open_console('/', ""),
        (KeyCode::Char('n'), false) => app.search_next(true),
        (KeyCode::Char('N'), false) => app.search_next(false),
        (KeyCode::Char('S'), false) => app.open_console(':', "shell "),
        (KeyCode::Char('r'), false) => app.open_console(':', "open_with "),
        (KeyCode::Char('?'), false) => app.help = Some(0),
        (KeyCode::Char('c'), false) => {
            *pending = Some('c');
            app.menu = Some(app::KeyMenu::change());
        }
        (KeyCode::Char('g'), false) => {
            *pending = Some('g');
            app.menu = Some(app::KeyMenu::go());
        }
        (KeyCode::Char('o'), false) => {
            *pending = Some('o');
            app.menu = Some(app::KeyMenu::sort());
        }
        (KeyCode::Char('y'), false) => {
            *pending = Some('y');
            app.menu = Some(app::KeyMenu::yank());
        }
        (KeyCode::Char('d'), false) => {
            *pending = Some('d');
            app.menu = Some(app::KeyMenu::cut());
        }
        (KeyCode::Char('p'), false) => {
            *pending = Some('p');
            app.menu = Some(app::KeyMenu::paste());
        }
        (KeyCode::Char('u'), false) => {
            *pending = Some('u');
            app.menu = Some(app::KeyMenu::un());
        }
        _ => {}
    }
}

/// Route a keypress to the open `:`/`/` console editor.
fn handle_console_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let Some(console) = app.console.as_mut() else {
        return;
    };
    match (key.code, ctrl) {
        (KeyCode::Esc, _) => app.console_cancel(),
        (KeyCode::Enter, _) => app.console_submit(),
        (KeyCode::Backspace, _) => console.backspace(),
        (KeyCode::Delete, _) => console.delete(),
        (KeyCode::Left, _) => console.left(),
        (KeyCode::Right, _) => console.right(),
        (KeyCode::Home, _) | (KeyCode::Char('a'), true) => console.home(),
        (KeyCode::End, _) | (KeyCode::Char('e'), true) => console.end(),
        (KeyCode::Char('u'), true) => console.clear_to_start(),
        (KeyCode::Char(c), false) => console.insert(c),
        _ => {}
    }
}

/// Scroll or close the help overlay. Scroll is clamped to the help text length;
/// the renderer clamps further to the visible viewport.
fn handle_help_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let cur = app.help.unwrap_or(0);
    let last = ui::help_len().saturating_sub(1);
    let set = |app: &mut App, v: usize| app.help = Some(v.min(last));
    match (key.code, ctrl) {
        (KeyCode::Char('q'), false)
        | (KeyCode::Char('Q'), false)
        | (KeyCode::Char('?'), false)
        | (KeyCode::Esc, _) => app.help = None,
        (KeyCode::Char('j'), false) | (KeyCode::Down, _) => set(app, cur + 1),
        (KeyCode::Char('k'), false) | (KeyCode::Up, _) => app.help = Some(cur.saturating_sub(1)),
        (KeyCode::Char('d'), true) | (KeyCode::PageDown, _) => set(app, cur + 10),
        (KeyCode::Char('u'), true) | (KeyCode::PageUp, _) => app.help = Some(cur.saturating_sub(10)),
        (KeyCode::Char('g'), false) | (KeyCode::Home, _) => app.help = Some(0),
        (KeyCode::Char('G'), false) | (KeyCode::End, _) => set(app, last),
        _ => {}
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::style::Color;

    #[test]
    fn osc_default_bg_only_for_rgb_themes() {
        // A concrete RGB theme background → OSC 11 with 2-hex-per-channel rgb:.
        assert_eq!(
            osc_set_default_bg(Color::Rgb { r: 0x28, g: 0x28, b: 0x28 }),
            Some("\x1b]11;rgb:28/28/28\x07".to_string()),
        );
        // The `default` theme keeps the terminal background → nothing to override.
        assert_eq!(osc_set_default_bg(Color::Reset), None);
        // Named/indexed colours aren't overridden either (no reliable RGB).
        assert_eq!(osc_set_default_bg(Color::Blue), None);
    }

    /// A detached program that has exited must be waited for, or it lingers as a
    /// zombie; one still running must be kept and left alone.
    #[test]
    fn reap_collects_finished_children_and_keeps_live_ones() {
        use std::process::{Command, Stdio};
        let spawn = |argv: &[&str]| {
            Command::new(argv[0])
                .args(&argv[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn")
        };
        let mut detached = vec![spawn(&["true"]), spawn(&["sleep", "30"])];
        let live_pid = detached[1].id();

        // `true` exits immediately; give it a moment, then reap until it is gone.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while detached.len() > 1 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            reap(&mut detached);
        }
        assert_eq!(detached.len(), 1, "the finished child was waited for");
        assert_eq!(detached[0].id(), live_pid, "the running child is kept");

        detached[0].kill().expect("kill");
        detached[0].wait().expect("wait");
    }

    /// Suspending for an editor must NOT redefine the terminal's default
    /// background unless the user opted in. Programs that choose a light/dark
    /// theme by querying OSC 11 (nvim, and auto appearance switchers) read that
    /// value; answering with our theme instead of the terminal's real color put
    /// them in an endless fight with a system-appearance-based switcher.
    #[test]
    fn suspend_does_not_redefine_terminal_bg_by_default() {
        let dark = Color::Rgb { r: 0x2e, g: 0x34, b: 0x40 };
        let set_bg = "\x1b]11;rgb:";

        let quiet = String::from_utf8(suspend_bytes(dark, false)).unwrap();
        assert!(!quiet.contains(set_bg), "default must not set OSC 11: {quiet:?}");
        // The cosmetic half still happens: we paint the screen in our own theme.
        assert!(quiet.contains("\x1b[2J"), "screen still cleared to the theme bg");

        // Opted in, the override is emitted...
        let loud = String::from_utf8(suspend_bytes(dark, true)).unwrap();
        assert!(loud.contains("\x1b]11;rgb:2e/34/40"), "opt-in sets OSC 11: {loud:?}");

        // ...and is undone on resume — but only when we actually set it, so we
        // never clobber a default background configured by something else.
        let resume_loud = String::from_utf8(resume_bytes(dark, true)).unwrap();
        assert!(resume_loud.contains("\x1b]111"), "opt-in resets OSC 11 on resume");
        let resume_quiet = String::from_utf8(resume_bytes(dark, false)).unwrap();
        assert!(!resume_quiet.contains("\x1b]111"), "never reset what we didn't set");

        // A non-RGB theme background has nothing to override either way.
        let reset_theme = String::from_utf8(resume_bytes(Color::Reset, true)).unwrap();
        assert!(!reset_theme.contains("\x1b]111"));
    }
}
