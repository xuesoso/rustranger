mod app;
mod config;
mod console;
mod fs;
mod image;
mod open;
mod ops;
mod preview;
mod state;
mod tab;
mod ui;
mod util;

use std::io::{self, Write};
use std::path::PathBuf;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};

use app::App;

struct Args {
    start: PathBuf,
    /// File-picker mode: write the chosen file's path here and exit.
    choosefile: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut start: Option<PathBuf> = None;
    let mut choosefile: Option<PathBuf> = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--choosefile" | "--selectfile" => choosefile = it.next().map(PathBuf::from),
            s if s.starts_with("--choosefile=") => {
                choosefile = s.split_once('=').map(|(_, v)| PathBuf::from(v))
            }
            "-h" | "--help" => {
                println!("usage: rustranger [PATH] [--choosefile FILE]");
                std::process::exit(0);
            }
            s if !s.starts_with('-') && start.is_none() => start = Some(PathBuf::from(s)),
            _ => {}
        }
    }
    let start = start.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    Args {
        start: start.canonicalize().unwrap_or(start),
        choosefile,
    }
}

fn main() -> io::Result<()> {
    let args = parse_args();
    let mut app = App::new(args.start);
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

fn setup_terminal() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, cursor::Hide)?;
    Ok(())
}

fn restore_terminal() -> io::Result<()> {
    let mut out = io::stdout();
    execute!(out, cursor::Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

fn run(app: &mut App) -> io::Result<()> {
    let mut out = io::stdout();
    let mut pending: Option<char> = None;
    let mut count: Option<usize> = None;
    while !app.quit {
        app.prepare_view();
        ui::draw(&mut out, app)?;

        if app.jobs_active() {
            // Poll with a short timeout so background-copy progress keeps updating.
            if event::poll(std::time::Duration::from_millis(80))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Release {
                        handle_key(app, key, &mut pending, &mut count);
                    }
                }
            }
            app.tick_jobs();
        } else {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    handle_key(app, key, &mut pending, &mut count)
                }
                _ => {}
            }
        }

        // Run any external program requested this iteration.
        if let Some(req) = app.pending_run.take() {
            run_external(&mut out, req)?;
        }
    }
    out.flush()?;
    Ok(())
}

/// Run an external program. Blocking programs (editors/pager/shell) suspend the
/// TUI; forked GUI programs are detached and the TUI keeps running.
fn run_external(out: &mut io::Stdout, req: open::RunRequest) -> io::Result<()> {
    use std::process::{Command, Stdio};
    if req.argv.is_empty() {
        return Ok(());
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
    } else {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = cmd.spawn();
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent, pending: &mut Option<char>, count: &mut Option<usize>) {
    use crate::fs::sort::SortKey;

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

    // A message is cleared on the next keypress.
    app.message = None;

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
                'o' => match c {
                    's' => app.set_sort(SortKey::Size),
                    'n' => app.set_sort(SortKey::Natural),
                    'b' => app.set_sort(SortKey::Basename),
                    'm' => app.set_sort(SortKey::Mtime),
                    'c' => app.set_sort(SortKey::Ctime),
                    'a' => app.set_sort(SortKey::Atime),
                    't' => app.set_sort(SortKey::Type),
                    'e' => app.set_sort(SortKey::Extension),
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
                'd' => match c {
                    'd' => app.cut(),
                    'D' => app.request_delete(),
                    _ => {}
                },
                'p' => match c {
                    'p' => app.paste(),
                    'l' => app.paste_links(true),
                    'L' => app.paste_links(false),
                    'h' => app.paste_hardlinks(),
                    _ => {}
                },
                'u' => {
                    if c == 'v' {
                        app.clear_marks()
                    }
                }
                'm' => app.set_bookmark(c),
                '`' | '\'' => app.enter_bookmark(c),
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
        (KeyCode::Char('m'), false) => *pending = Some('m'),
        (KeyCode::Char('`'), false) | (KeyCode::Char('\''), false) => *pending = Some('`'),
        (KeyCode::Char('j'), false) | (KeyCode::Down, false) => app.move_cursor(n as isize),
        (KeyCode::Char('k'), false) | (KeyCode::Up, false) => app.move_cursor(-(n as isize)),
        (KeyCode::Char('d'), true) => app.move_cursor(10),
        (KeyCode::Char('u'), true) => app.move_cursor(-10),
        (KeyCode::Char('h'), false) | (KeyCode::Left, false) => app.ascend(),
        (KeyCode::Char('l'), false) | (KeyCode::Right, false) | (KeyCode::Enter, _) => app.enter(),
        (KeyCode::Char('G'), false) => app.move_to_bottom(),
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
        (KeyCode::Char('c'), false) => *pending = Some('c'),
        (KeyCode::Char('g'), false) => *pending = Some('g'),
        (KeyCode::Char('o'), false) => *pending = Some('o'),
        (KeyCode::Char('y'), false) => *pending = Some('y'),
        (KeyCode::Char('d'), false) => *pending = Some('d'),
        (KeyCode::Char('p'), false) => *pending = Some('p'),
        (KeyCode::Char('u'), false) => *pending = Some('u'),
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

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
