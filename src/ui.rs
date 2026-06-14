// Miller-columns renderer (parent | current | preview) with borders and bars.
// Ported in spirit from ranger/gui/widgets/view_miller.py + browsercolumn.py.

use std::io::{self, Write};

use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{cursor::MoveTo, queue};

use crate::app::App;
use crate::fs::{Dir, Entry, FType};
use crate::util;

const BORDER: Color = Color::DarkGrey;

pub fn draw(out: &mut impl Write, app: &App) -> io::Result<()> {
    let (cols, rows) = crossterm::terminal::size()?;
    let (cols, rows) = (cols as usize, rows as usize);
    if rows < 3 || cols < 4 {
        return Ok(());
    }

    queue!(out, Clear(ClearType::All))?;
    if app.console.is_none() {
        queue!(out, crossterm::cursor::Hide)?;
    }
    draw_titlebar(out, app, cols)?;

    let body_top = 1u16;
    let body_height = rows - 2;
    draw_miller(out, app, body_top, body_height, cols)?;
    if app.console.is_some() {
        draw_console(out, app, rows as u16 - 1, cols)?;
    } else {
        draw_statusbar(out, app, rows as u16 - 1, cols)?;
    }

    out.flush()?;
    Ok(())
}

fn draw_console(out: &mut impl Write, app: &App, y: u16, cols: usize) -> io::Result<()> {
    let Some(console) = &app.console else {
        return Ok(());
    };
    let line = format!("{}{}", console.prompt, console.input);
    queue!(
        out,
        MoveTo(0, y),
        Clear(ClearType::CurrentLine),
        Print(util::truncate(&line, cols)),
    )?;
    // Place the hardware cursor at the edit position and show it.
    let cursor_x = (1 + console.cursor).min(cols.saturating_sub(1)) as u16;
    queue!(out, MoveTo(cursor_x, y), crossterm::cursor::Show)?;
    Ok(())
}

// ---- title bar -------------------------------------------------------------

fn draw_titlebar(out: &mut impl Write, app: &App, cols: usize) -> io::Result<()> {
    // Right side: tab indicators (only shown when more than one tab is open).
    let tabs = if app.tab_count() > 1 {
        let mut s = String::new();
        for i in 0..app.tab_count() {
            if i == app.current_tab {
                s.push_str(&format!("[{}]", i + 1));
            } else {
                s.push_str(&format!(" {} ", i + 1));
            }
        }
        s
    } else {
        String::new()
    };
    let tabs_w = util::display_width(&tabs);
    let path_budget = cols.saturating_sub(tabs_w + 1);

    let cwd = app.cwd();
    let path = cwd.to_string_lossy();
    let (head, tail) = split_breadcrumb(&path);
    let head = util::truncate(&head, path_budget);
    queue!(
        out,
        MoveTo(0, 0),
        SetForegroundColor(Color::Blue),
        SetAttribute(Attribute::Bold),
        Print(&head),
    )?;
    if util::display_width(&head) + util::display_width(&tail) <= path_budget {
        queue!(out, SetForegroundColor(Color::White), Print(&tail))?;
    }
    queue!(out, SetAttribute(Attribute::Reset))?;

    if !tabs.is_empty() {
        queue!(
            out,
            MoveTo((cols - tabs_w) as u16, 0),
            SetForegroundColor(Color::Cyan),
            Print(&tabs),
        )?;
    }
    queue!(out, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

/// Split a path into ("/parent/dirs/", "basename") for breadcrumb styling.
fn split_breadcrumb(path: &str) -> (String, String) {
    if path == "/" {
        return (String::new(), "/".to_string());
    }
    match path.rsplit_once('/') {
        Some((head, tail)) => (format!("{}/", head), tail.to_string()),
        None => (String::new(), path.to_string()),
    }
}

// ---- miller layout ---------------------------------------------------------

fn draw_miller(
    out: &mut impl Write,
    app: &App,
    top: u16,
    height: usize,
    cols: usize,
) -> io::Result<()> {
    let ratios = &app.settings.column_ratios;
    let layout = column_layout(cols, ratios);
    let n = layout.len();

    for (i, &(x, width)) in layout.iter().enumerate() {
        let is_preview = i + 1 == n && n > 1;
        let is_main = i + 1 == n - 1 || (n == 1);

        if is_preview {
            draw_preview_column(out, app, x, top, width, height)?;
        } else if is_main {
            draw_filelist(out, app.current_dir(), &app.tags, x, top, width, height, true)?;
        } else {
            // Parent column(s): for a 3-column layout, just the immediate parent.
            if let Some(p) = app.parent_path().and_then(|p| app.get_cached(&p).map(|_| p)) {
                let dir = app.get_cached(&p).unwrap();
                draw_filelist(out, dir, &app.tags, x, top, width, height, true)?;
            }
        }

        // Draw the separator/border to the right of this column.
        if i + 1 < n {
            let bx = (x + width) as u16;
            if app.settings.draw_borders {
                draw_vline(out, bx, top, height)?;
            }
        }
    }
    Ok(())
}

/// Distribute `cols` across the ratio list, leaving 1 gap column between panes.
fn column_layout(cols: usize, ratios: &[u32]) -> Vec<(usize, usize)> {
    let n = ratios.len().max(1);
    let gaps = n - 1;
    let total: u32 = ratios.iter().sum::<u32>().max(1);
    let usable = cols.saturating_sub(gaps);

    let mut widths: Vec<usize> = ratios
        .iter()
        .map(|&r| (usable * r as usize) / total as usize)
        .collect();
    // Hand any rounding remainder to the (main) column second-from-right.
    let assigned: usize = widths.iter().sum();
    if usable > assigned {
        let idx = n.saturating_sub(2);
        widths[idx] += usable - assigned;
    }

    let mut out = Vec::with_capacity(n);
    let mut x = 0usize;
    for w in widths {
        out.push((x, w));
        x += w + 1; // +1 for the gap/border column
    }
    out
}

fn draw_preview_column(
    out: &mut impl Write,
    app: &App,
    x: usize,
    top: u16,
    width: usize,
    height: usize,
) -> io::Result<()> {
    let selected = app.current_dir().current();
    let Some(entry) = selected else {
        return Ok(());
    };

    if entry.is_dir() && entry.accessible {
        if let Some(dir) = app.get_cached(&entry.path) {
            draw_filelist(out, dir, &app.tags, x, top, width, height, false)?;
        }
    } else if matches!(entry.ftype, FType::File) {
        draw_file_preview(out, app, x, top, width, height)?;
    } else {
        let info = format!("{:?}", entry.ftype);
        queue!(
            out,
            MoveTo(x as u16, top),
            SetForegroundColor(Color::DarkGrey),
            Print(util::truncate(&info, width)),
            ResetColor,
        )?;
    }
    Ok(())
}

fn draw_file_preview(
    out: &mut impl Write,
    app: &App,
    x: usize,
    top: u16,
    width: usize,
    height: usize,
) -> io::Result<()> {
    use crate::preview::Preview;
    match app.current_preview() {
        Some(Preview::Text(lines)) => {
            let start = app.preview_scroll.min(lines.len().saturating_sub(1));
            for (row, line) in lines.iter().skip(start).take(height).enumerate() {
                queue!(
                    out,
                    MoveTo(x as u16, top + row as u16),
                    Print(util::truncate(line, width)),
                )?;
            }
        }
        Some(Preview::Binary) => placeholder(out, x, top, width, "(binary file)", Color::DarkGrey)?,
        Some(Preview::TooBig) => {
            placeholder(out, x, top, width, "(file too large to preview)", Color::DarkGrey)?
        }
        Some(Preview::Empty) => placeholder(out, x, top, width, "(empty)", Color::Yellow)?,
        Some(Preview::Error(e)) => {
            placeholder(out, x, top, width, &format!("(error: {})", e), Color::Red)?
        }
        None => {}
    }
    Ok(())
}

fn placeholder(
    out: &mut impl Write,
    x: usize,
    top: u16,
    width: usize,
    msg: &str,
    color: Color,
) -> io::Result<()> {
    queue!(
        out,
        MoveTo(x as u16, top),
        SetForegroundColor(color),
        Print(util::truncate(msg, width)),
        ResetColor,
    )
}

// ---- file list column ------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn draw_filelist(
    out: &mut impl Write,
    dir: &Dir,
    tags: &crate::state::tags::Tags,
    x: usize,
    top: u16,
    width: usize,
    height: usize,
    focused: bool,
) -> io::Result<()> {
    if let Some(err) = &dir.error {
        queue!(
            out,
            MoveTo(x as u16, top),
            SetForegroundColor(Color::Red),
            Print(util::truncate(&format!("error: {}", err), width)),
            ResetColor,
        )?;
        return Ok(());
    }
    if dir.is_empty() {
        queue!(
            out,
            MoveTo(x as u16, top),
            SetForegroundColor(Color::Yellow),
            Print(util::truncate("(empty)", width)),
            ResetColor,
        )?;
        return Ok(());
    }

    let offset = scroll_offset(dir.pointer, dir.len(), height, 4);
    for row in 0..height {
        let idx = offset + row;
        if idx >= dir.len() {
            break;
        }
        let entry = dir.entry_at(idx).unwrap();
        let selected = focused && idx == dir.pointer;
        let tag = tags.marker(&entry.path);
        draw_entry_row(out, entry, tag, selected, x, top + row as u16, width)?;
    }
    Ok(())
}

fn draw_entry_row(
    out: &mut impl Write,
    entry: &Entry,
    tag: Option<char>,
    selected: bool,
    x: usize,
    y: u16,
    width: usize,
) -> io::Result<()> {
    let (color, bold) = entry_style(entry);
    // Column 0 shows the mark (*), the tag symbol, or a space.
    let marker: String = if entry.marked {
        "*".to_string()
    } else if let Some(t) = tag {
        t.to_string()
    } else {
        " ".to_string()
    };
    let info = info_string(entry);
    let info_w = util::display_width(&info);

    let name_budget = width.saturating_sub(info_w + 2);
    let name = util::truncate(&entry.name, name_budget);

    queue!(out, MoveTo(x as u16, y))?;
    if selected {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    if bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    queue!(out, SetForegroundColor(color))?;

    let left = format!("{}{}", marker, name);
    queue!(out, Print(&left))?;

    let used = util::display_width(&left);
    if used + info_w < width {
        let pad = width - used - info_w;
        queue!(out, Print(" ".repeat(pad)), Print(&info))?;
    } else if used < width {
        queue!(out, Print(" ".repeat(width - used)))?;
    }

    queue!(out, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

fn entry_style(entry: &Entry) -> (Color, bool) {
    if !entry.accessible || (entry.is_link && !entry.link_ok) {
        return (Color::Red, false);
    }
    match entry.ftype {
        FType::Dir => (Color::Blue, true),
        FType::Symlink => (Color::Cyan, false),
        FType::Fifo | FType::Socket => (Color::Magenta, false),
        FType::BlockDevice | FType::CharDevice => (Color::Yellow, true),
        FType::File if entry.executable => (Color::Green, true),
        _ => (Color::Reset, false),
    }
}

fn info_string(entry: &Entry) -> String {
    match entry.ftype {
        FType::Dir => String::new(),
        _ => util::human_size(entry.size),
    }
}

// ---- status bar ------------------------------------------------------------

fn draw_statusbar(out: &mut impl Write, app: &App, y: u16, cols: usize) -> io::Result<()> {
    let dir = app.current_dir();

    // While a background copy/move runs, show an aggregate progress bar instead.
    if app.jobs_active() {
        let (done, total) = app.jobs.iter().fold((0u64, 0u64), |(d, t), j| {
            (d + j.progress().done, t + j.progress().total)
        });
        let pct = done.saturating_mul(100).checked_div(total).unwrap_or(0).min(100);
        let bar_w = 20usize.min(cols.saturating_sub(30));
        let filled = bar_w * pct as usize / 100;
        let bar: String = "█".repeat(filled) + &"░".repeat(bar_w.saturating_sub(filled));
        let label = app
            .jobs
            .iter()
            .map(|j| j.progress().label.as_str())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        let text = format!(
            " {} [{}] {}% {} / {} {}",
            if app.jobs.iter().any(|j| j.cut) { "moving" } else { "copying" },
            bar,
            pct,
            util::human_size(done),
            util::human_size(total),
            label,
        );
        queue!(
            out,
            MoveTo(0, y),
            SetForegroundColor(Color::Green),
            Print(util::truncate(&text, cols)),
            ResetColor,
        )?;
        return Ok(());
    }

    let left = if let Some(msg) = &app.message {
        msg.clone()
    } else if let Some(entry) = dir.current() {
        let link = if entry.is_link {
            format!(" -> {}", target_of(entry))
        } else {
            String::new()
        };
        format!(
            "{} {} {} {}{}",
            util::permission_string(entry.mode),
            util::username(entry.uid),
            util::groupname(entry.gid),
            util::human_size(entry.size),
            link,
        )
    } else {
        String::new()
    };

    let right = if dir.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", dir.pointer + 1, dir.len())
    };

    let left = util::truncate(&left, cols.saturating_sub(right.len() + 1));
    let used = util::display_width(&left);
    let pad = cols.saturating_sub(used + right.len());

    queue!(
        out,
        MoveTo(0, y),
        SetForegroundColor(Color::Grey),
        Print(left),
        Print(" ".repeat(pad)),
        SetForegroundColor(Color::Blue),
        Print(right),
        ResetColor,
    )?;
    Ok(())
}

fn target_of(entry: &Entry) -> String {
    std::fs::read_link(&entry.path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---- helpers ---------------------------------------------------------------

fn draw_vline(out: &mut impl Write, x: u16, top: u16, height: usize) -> io::Result<()> {
    queue!(out, SetForegroundColor(BORDER))?;
    for row in 0..height {
        queue!(out, MoveTo(x, top + row as u16), Print("│"))?;
    }
    queue!(out, ResetColor)?;
    Ok(())
}

fn scroll_offset(pointer: usize, len: usize, height: usize, margin: usize) -> usize {
    if len <= height {
        return 0;
    }
    let margin = margin.min(height / 2);
    let max_offset = len - height;
    let lower = pointer.saturating_sub(margin);
    let upper = (pointer + margin + 1).saturating_sub(height);
    lower.max(upper).min(max_offset)
}
