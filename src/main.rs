use std::io::{self, Write};
use std::path::PathBuf;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};

// Modules live in the library crate (src/lib.rs); the binary just drives them.
use rustranger::app::{self, App};
use rustranger::{config, fs, open, ui};

/// Internal pending-key sentinel for the `um{key}` chord (awaiting the bookmark
/// key to delete). Not a typeable key, so it never collides with a real prefix.
const UNBOOKMARK_PENDING: char = '\u{1}';

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
  --theme NAME              default|gruvbox-dark|gruvbox-light|solarized-dark|
                            solarized-light|nord|dracula|subliminal|gitlab-dark|
                            gitlab-light|everforest-dark|everforest-light|
                            one-light|ayu-light
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

    setup_terminal()?;
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
    execute!(out, crossterm::terminal::EnableLineWrap, cursor::Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
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
    while !app.quit {
        app.prepare_view();
        if let Ok((cols, rows)) = crossterm::terminal::size() {
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
                screen::clear(&mut frame, default)?;
                out.write_all(&frame)?;
                needs_full = false;
            }
            let cursor = ui::render(&mut cur, app);
            frame.clear();
            screen::flush(&mut frame, prev.as_ref(), &cur)?;
            // Cursor: shown at the console edit position, hidden otherwise.
            match cursor {
                Some((cx, cy)) => {
                    crossterm::queue!(frame, crossterm::cursor::MoveTo(cx, cy), crossterm::cursor::Show)?
                }
                None => crossterm::queue!(frame, crossterm::cursor::Hide)?,
            }
            out.write_all(&frame)?;
            out.flush()?;
            prev = Some(cur.clone());
        }

        // Wait for input: poll briefly when background jobs run (so progress
        // updates), otherwise block until a key arrives.
        let have_event = if app.jobs_active() {
            event::poll(std::time::Duration::from_millis(80))?
        } else {
            true
        };
        if have_event {
            handle_event(app, event::read()?, &mut pending, &mut count);
            // Coalesce a burst: handle every event already queued before the next
            // redraw, so fast key-repeat (which backs up behind a slow tmux/SSH
            // redraw) collapses into ONE frame instead of one frame per key. This
            // self-paces rendering to whatever the display can keep up with.
            while event::poll(std::time::Duration::ZERO)? {
                handle_event(app, event::read()?, &mut pending, &mut count);
                // Don't keep swallowing keys once a key has asked to launch an
                // external program or quit (those keys belong to the next state).
                if app.pending_run.is_some() || app.quit {
                    break;
                }
            }
        }
        if app.jobs_active() {
            app.tick_jobs();
        }

        // Run any external program requested this iteration. A blocking program
        // (editor/pager/shell) suspends the TUI, so force a full repaint after.
        if let Some(req) = app.pending_run.take() {
            if run_external(&mut out, req)? {
                needs_full = true;
            }
        }
    }
    out.flush()?;
    Ok(())
}

/// Dispatch a single terminal event (ignores key releases and non-key events;
/// resize is handled implicitly since the loop re-queries the size each frame).
fn handle_event(app: &mut App, ev: Event, pending: &mut Option<char>, count: &mut Option<usize>) {
    if let Event::Key(key) = ev {
        if key.kind != KeyEventKind::Release {
            handle_key(app, key, pending, count);
        }
    }
}

/// Run an external program. Blocking programs (editors/pager/shell) suspend the
/// TUI; forked GUI programs are detached and the TUI keeps running.
///
/// Returns `true` when the TUI was suspended (a blocking program ran), so the
/// caller knows the terminal contents are now unknown and the next frame must be
/// a full repaint rather than a diff against the pre-launch screen.
fn run_external(out: &mut io::Stdout, req: open::RunRequest) -> io::Result<bool> {
    use std::process::{Command, Stdio};
    if req.argv.is_empty() {
        return Ok(false);
    }
    let mut cmd = Command::new(&req.argv[0]);
    cmd.args(&req.argv[1..]).current_dir(&req.cwd);

    if req.block {
        restore_terminal()?;
        let status = cmd.status();
        setup_terminal()?;
        out.flush()?;
        if let Err(e) = status {
            // Swallow; nothing fatal, surface nothing for now.
            let _ = e;
        }
        Ok(true)
    } else {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = cmd.spawn();
        Ok(false)
    }
}

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
                    *count = Some(count.unwrap_or(0) * 10 + d);
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
                'y' => {
                    if c == 'y' {
                        app.copy()
                    }
                }
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
        (KeyCode::Char('z'), false) => app.toggle_hidden(),
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
