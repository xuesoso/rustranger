// Miller-columns renderer (parent | current | preview) with borders and bars.
// Ported in spirit from ranger/gui/widgets/view_miller.py + browsercolumn.py.
//
// Colors come from the active `Theme`. The whole screen is painted with the
// theme background each frame (so light themes are genuinely light), and every
// drawn element sets both foreground and background before printing so a trailing
// reset can never leak the terminal-default background into the next cells.

use std::io::{self, Write};

use crossterm::style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{cursor::MoveTo, queue};

use crate::app::App;
use crate::config::{Settings, SizeFormat, TimeFormat, TimeType};
use crate::fs::{Dir, Entry, FType};
use crate::theme::Theme;
use crate::util;

/// Minimum display width reserved for a file/folder name before the date/size
/// info column is allowed to take space (the info is dropped in narrower columns).
const MIN_NAME_WIDTH: usize = 24;

pub fn draw(out: &mut impl Write, app: &App) -> io::Result<()> {
    let (cols, rows) = crossterm::terminal::size()?;
    let (cols, rows) = (cols as usize, rows as usize);
    if rows < 3 || cols < 4 {
        return Ok(());
    }
    let t = &app.settings.theme;

    // Paint the entire screen with the theme background.
    queue!(
        out,
        SetForegroundColor(t.fg),
        SetBackgroundColor(t.bg),
        Clear(ClearType::All),
    )?;
    if app.console.is_none() {
        queue!(out, crossterm::cursor::Hide)?;
    }
    draw_titlebar(out, app, cols, t)?;

    let body_top = 1u16;
    let body_height = rows - 2;

    // The help overlay replaces the browser view entirely while open.
    if let Some(scroll) = app.help {
        draw_help(out, scroll, body_top, body_height, cols, t)?;
        out.flush()?;
        return Ok(());
    }

    draw_miller(out, app, body_top, body_height, cols, t)?;
    if app.console.is_some() {
        draw_console(out, app, rows as u16 - 1, cols, t)?;
    } else {
        draw_statusbar(out, app, rows as u16 - 1, cols, t)?;
    }

    // A pending key-chain hint (e.g. the sort menu) overlays everything else.
    if let Some(menu) = &app.menu {
        draw_menu(out, menu, cols, rows, t)?;
    }

    out.flush()?;
    Ok(())
}

/// Pad `s` with trailing spaces to a target display width (no-op if already wider).
fn pad_right(s: &str, width: usize) -> String {
    let w = util::display_width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - w))
    }
}

/// Draw a key-chain hint menu as a bordered popup anchored to the bottom-right,
/// just above the status bar. Long lists are clamped to the available height.
fn draw_menu(
    out: &mut impl Write,
    menu: &crate::app::KeyMenu,
    cols: usize,
    rows: usize,
    t: &Theme,
) -> io::Result<()> {
    // Clamp the row count to what fits (title bar + top/title/bottom + status bar).
    let max_items = rows.saturating_sub(5).max(1);
    let mut items: Vec<(String, String)> = menu.items.clone();
    if items.len() > max_items {
        items.truncate(max_items.saturating_sub(1));
        items.push((String::new(), "…".to_string()));
    }

    // Widest content row decides the interior width (key column + 2 + description).
    let key_w = items.iter().map(|(k, _)| util::display_width(k)).max().unwrap_or(1);
    let interior = items
        .iter()
        .map(|(_, d)| key_w + 2 + util::display_width(d))
        .chain(std::iter::once(util::display_width(&menu.title)))
        .max()
        .unwrap_or(0)
        .min(cols.saturating_sub(5)); // keep the box on-screen

    let box_w = interior + 4; // 2 border cells + 1 space padding each side
    let box_h = items.len() + 3; // top border + title + items + bottom border

    // Anchor bottom-right, leaving the status line (rows-1) clear; clamp on small terminals.
    let x = cols.saturating_sub(box_w + 1);
    let top = rows.saturating_sub(box_h + 1).max(1);
    let avail = cols.saturating_sub(x);

    let hline = "─".repeat(interior + 2);

    // Top border.
    queue!(
        out,
        MoveTo(x as u16, top as u16),
        SetBackgroundColor(t.bg),
        SetForegroundColor(t.border),
        Print(util::truncate(&format!("┌{}┐", hline), avail)),
    )?;
    // Title row (bold, in the title color, between border edges).
    queue!(
        out,
        MoveTo(x as u16, (top + 1) as u16),
        SetBackgroundColor(t.bg),
        SetForegroundColor(t.border),
        Print("│ "),
        SetForegroundColor(t.title),
        SetAttribute(Attribute::Bold),
        Print(util::truncate(&pad_right(&menu.title, interior), interior)),
        SetAttribute(Attribute::NormalIntensity),
        SetForegroundColor(t.border),
        Print(" │"),
    )?;
    // One row per item: right-aligned key, two spaces, description.
    for (i, (key, desc)) in items.iter().enumerate() {
        let pad = " ".repeat(key_w.saturating_sub(util::display_width(key)));
        let row = format!("{}{}  {}", pad, key, desc);
        queue!(
            out,
            MoveTo(x as u16, (top + 2 + i) as u16),
            SetBackgroundColor(t.bg),
            SetForegroundColor(t.border),
            Print("│ "),
            SetForegroundColor(t.accent),
            Print(util::truncate(&pad_right(&row, interior), interior)),
            SetForegroundColor(t.border),
            Print(" │"),
        )?;
    }
    // Bottom border.
    queue!(
        out,
        MoveTo(x as u16, (top + box_h - 1) as u16),
        SetBackgroundColor(t.bg),
        SetForegroundColor(t.border),
        Print(util::truncate(&format!("└{}┘", hline), avail)),
        ResetColor,
    )?;
    Ok(())
}

// ---- help overlay ----------------------------------------------------------

/// The help text shown by `?`. Lines with no leading space are section headers
/// (rendered bold); indented lines are `keys  description` entries.
const HELP: &[&str] = &[
    "rustranger — key bindings",
    "",
    "Navigation",
    "  j / k   ↓ / ↑    move cursor (prefix a count, e.g. 3j)",
    "  h / l   ← / →    parent dir / enter dir or open file (also Enter)",
    "  gg / G            jump to top / bottom",
    "  Ctrl-d / Ctrl-u   half-page down / up",
    "  gh / g/           go to home (~) / root (/)",
    "  H / L             history back / forward",
    "  J / K             scroll the preview pane",
    "",
    "Selection & marks",
    "  space             mark entry and move down",
    "  v                 visual-select mode",
    "  V                 toggle all marks",
    "  uv                clear all marks",
    "  Esc               clear marks / cancel count",
    "",
    "File operations",
    "  yy                copy selection",
    "  dd                cut selection",
    "  pp                paste",
    "  pl / pL           paste symlink (relative / absolute)",
    "  ph                paste hardlink",
    "  D                 delete selection (with confirm)",
    "  cw                rename",
    "",
    "Sorting  (press o, then…  UPPERCASE = reversed)",
    "  os / oS           by size",
    "  on / oN           natural",
    "  ob / oB           basename",
    "  om / oM           mtime",
    "  oc / oC           ctime",
    "  oa / oA           atime",
    "  ot / oT           type",
    "  oe / oE           extension",
    "  oz                random",
    "  or                toggle reverse",
    "  of                toggle directories-first",
    "",
    "Tabs",
    "  Tab / BackTab     next / previous tab",
    "  gn                new tab",
    "  gt / gT           next / previous tab",
    "  Alt-1 … Alt-9     go to tab N",
    "  q / Q             close tab / quit all",
    "",
    "Bookmarks & tags",
    "  m{key}            set bookmark",
    "  `{key} / '{key}   go to bookmark",
    "  um{key}           delete bookmark  (:clearbookmarks clears all)",
    "  t                 toggle tag on selection",
    "",
    "View, search & console",
    "  z                 toggle hidden files",
    "  / , n / N         search, next / previous match",
    "  :                 command console (mkdir, touch, chmod, filter, set, …)",
    "  S                 run a shell command",
    "  r                 open selection with a program",
    "  ?                 this help",
];

/// Number of help lines (used by the key handler to clamp scrolling).
pub fn help_len() -> usize {
    HELP.len()
}

/// Draw the scrollable help overlay as a full-width bordered panel.
fn draw_help(
    out: &mut impl Write,
    scroll: usize,
    top: u16,
    height: usize,
    cols: usize,
    t: &Theme,
) -> io::Result<()> {
    let top = top as usize;
    let inner_w = cols.saturating_sub(4); // text area between "│ " and " │"
    let inner_h = height.saturating_sub(2); // minus top + bottom border rows
    let max_scroll = HELP.len().saturating_sub(inner_h);
    let scroll = scroll.min(max_scroll);

    // Top border carries the title and the scroll hint.
    let mut tb = String::from("┌─ Help (j/k scroll · g/G top/bottom · q close) ");
    let used = util::display_width(&tb);
    if used + 1 < cols {
        tb.push_str(&"─".repeat(cols - 1 - used));
    }
    tb.push('┐');
    queue!(
        out,
        MoveTo(0, top as u16),
        SetBackgroundColor(t.bg),
        SetForegroundColor(t.border),
        Print(util::truncate(&tb, cols)),
    )?;

    for row in 0..inner_h {
        let y = (top + 1 + row) as u16;
        queue!(
            out,
            MoveTo(0, y),
            SetBackgroundColor(t.bg),
            SetForegroundColor(t.border),
            Print("│ "),
        )?;
        if let Some(line) = HELP.get(scroll + row) {
            let header = !line.is_empty() && !line.starts_with(' ');
            if header {
                queue!(out, SetForegroundColor(t.title), SetAttribute(Attribute::Bold))?;
            } else {
                queue!(out, SetForegroundColor(t.fg))?;
            }
            queue!(
                out,
                Print(pad_right(&util::truncate(line, inner_w), inner_w)),
                SetAttribute(Attribute::NormalIntensity),
            )?;
        } else {
            queue!(out, Print(" ".repeat(inner_w)))?;
        }
        queue!(out, SetForegroundColor(t.border), Print(" │"))?;
    }

    // Bottom border.
    queue!(
        out,
        MoveTo(0, (top + height - 1) as u16),
        SetBackgroundColor(t.bg),
        SetForegroundColor(t.border),
        Print(util::truncate(&format!("└{}┘", "─".repeat(cols.saturating_sub(2))), cols)),
        ResetColor,
    )?;
    Ok(())
}

fn draw_console(out: &mut impl Write, app: &App, y: u16, cols: usize, t: &Theme) -> io::Result<()> {
    let Some(console) = &app.console else {
        return Ok(());
    };
    let line = format!("{}{}", console.prompt, console.input);
    queue!(
        out,
        MoveTo(0, y),
        SetForegroundColor(t.fg),
        SetBackgroundColor(t.bg),
        Clear(ClearType::CurrentLine),
        Print(util::truncate(&line, cols)),
    )?;
    // Place the hardware cursor at the edit position and show it.
    let cursor_x = (1 + console.cursor).min(cols.saturating_sub(1)) as u16;
    queue!(out, MoveTo(cursor_x, y), crossterm::cursor::Show)?;
    Ok(())
}

// ---- title bar -------------------------------------------------------------

fn draw_titlebar(out: &mut impl Write, app: &App, cols: usize, t: &Theme) -> io::Result<()> {
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
        SetBackgroundColor(t.bg),
        SetForegroundColor(t.title),
        SetAttribute(Attribute::Bold),
        Print(&head),
    )?;
    if util::display_width(&head) + util::display_width(&tail) <= path_budget {
        queue!(
            out,
            SetAttribute(Attribute::NormalIntensity),
            SetForegroundColor(t.fg),
            Print(&tail),
        )?;
    }
    queue!(out, SetAttribute(Attribute::Reset))?;

    if !tabs.is_empty() {
        queue!(
            out,
            MoveTo((cols - tabs_w) as u16, 0),
            SetBackgroundColor(t.bg),
            SetForegroundColor(t.accent),
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
    t: &Theme,
) -> io::Result<()> {
    let ratios = &app.settings.column_ratios;
    let layout = column_layout(cols, ratios);
    let n = layout.len();

    for (i, &(x, width)) in layout.iter().enumerate() {
        let is_preview = i + 1 == n && n > 1;
        let is_main = i + 1 == n - 1 || (n == 1);

        if is_preview {
            draw_preview_column(out, app, x, top, width, height, t)?;
        } else if is_main {
            // The date column is shown only in the current (main) column.
            draw_filelist(
                out,
                app.current_dir(),
                &app.tags,
                &app.settings,
                app.settings.show_date,
                x,
                top,
                width,
                height,
                true,
                t,
            )?;
        } else {
            // Parent column(s): for a 3-column layout, just the immediate parent.
            if let Some(p) = app.parent_path().and_then(|p| app.get_cached(&p).map(|_| p)) {
                let dir = app.get_cached(&p).unwrap();
                draw_filelist(out, dir, &app.tags, &app.settings, false, x, top, width, height, true, t)?;
            }
        }

        // Draw the separator/border to the right of this column.
        if i + 1 < n {
            let bx = (x + width) as u16;
            if app.settings.draw_borders {
                draw_vline(out, bx, top, height, t)?;
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
    t: &Theme,
) -> io::Result<()> {
    let selected = app.current_dir().current();
    let Some(entry) = selected else {
        return Ok(());
    };

    if entry.is_dir() && entry.accessible {
        if let Some(dir) = app.get_cached(&entry.path) {
            draw_filelist(out, dir, &app.tags, &app.settings, false, x, top, width, height, false, t)?;
        }
    } else if matches!(entry.ftype, FType::File) {
        draw_file_preview(out, app, x, top, width, height, t)?;
    } else {
        let info = format!("{:?}", entry.ftype);
        queue!(
            out,
            MoveTo(x as u16, top),
            SetBackgroundColor(t.bg),
            SetForegroundColor(t.info),
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
    t: &Theme,
) -> io::Result<()> {
    use crate::preview::Preview;
    match app.current_preview() {
        Some(Preview::Text(lines)) => {
            let start = app.preview_scroll.min(lines.len().saturating_sub(1));
            for (row, line) in lines.iter().skip(start).take(height).enumerate() {
                queue!(
                    out,
                    MoveTo(x as u16, top + row as u16),
                    SetBackgroundColor(t.bg),
                    SetForegroundColor(t.fg),
                    Print(util::truncate(line, width)),
                )?;
            }
        }
        Some(Preview::Binary) => placeholder(out, x, top, width, "(binary file)", t.info, t)?,
        Some(Preview::TooBig) => {
            placeholder(out, x, top, width, "(file too large to preview)", t.info, t)?
        }
        Some(Preview::Empty) => placeholder(out, x, top, width, "(empty)", t.warning, t)?,
        Some(Preview::Error(e)) => {
            placeholder(out, x, top, width, &format!("(error: {})", e), t.error, t)?
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
    t: &Theme,
) -> io::Result<()> {
    queue!(
        out,
        MoveTo(x as u16, top),
        SetBackgroundColor(t.bg),
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
    settings: &Settings,
    with_time: bool,
    x: usize,
    top: u16,
    width: usize,
    height: usize,
    focused: bool,
    t: &Theme,
) -> io::Result<()> {
    if let Some(err) = &dir.error {
        queue!(
            out,
            MoveTo(x as u16, top),
            SetBackgroundColor(t.bg),
            SetForegroundColor(t.error),
            Print(util::truncate(&format!("error: {}", err), width)),
            ResetColor,
        )?;
        return Ok(());
    }
    if dir.is_empty() {
        queue!(
            out,
            MoveTo(x as u16, top),
            SetBackgroundColor(t.bg),
            SetForegroundColor(t.warning),
            Print(util::truncate("(empty)", width)),
            ResetColor,
        )?;
        return Ok(());
    }

    let offset = scroll_begin(dir.pointer, dir.len(), height);
    for row in 0..height {
        let idx = offset + row;
        if idx >= dir.len() {
            break;
        }
        let entry = dir.entry_at(idx).unwrap();
        let selected = focused && idx == dir.pointer;
        let tag = tags.marker(&entry.path);
        draw_entry_row(out, entry, tag, selected, settings, with_time, x, top + row as u16, width, t)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_entry_row(
    out: &mut impl Write,
    entry: &Entry,
    tag: Option<char>,
    selected: bool,
    settings: &Settings,
    with_time: bool,
    x: usize,
    y: u16,
    width: usize,
    t: &Theme,
) -> io::Result<()> {
    let (color, type_bold) = entry_style(entry, t);
    // Marked (spacebar) and tagged (t) entries are shown as a full inverted row.
    let marked = entry.marked || tag.is_some();
    // The cursor row is also inverted; both it and marks reverse the colors.
    let highlighted = selected || marked;

    // The file name gets priority: reserve it at least MIN_NAME_WIDTH columns and
    // only show the date/size info block when it still fits beyond that minimum.
    // In narrow columns the info is dropped so names stay readable.
    let info = info_string(entry, settings, with_time);
    let info = if width >= MIN_NAME_WIDTH + util::display_width(&info) + 2 {
        info
    } else {
        String::new()
    };
    let info_w = util::display_width(&info);

    let gutter_and_gap = if info.is_empty() { 1 } else { info_w + 2 };
    let name_budget = width.saturating_sub(gutter_and_gap);
    let name = util::truncate(&entry.name, name_budget);

    // Base colors first; Reverse then swaps them so the row becomes a colored bar.
    queue!(out, MoveTo(x as u16, y), SetBackgroundColor(t.bg), SetForegroundColor(color))?;
    if highlighted {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    // On an inverted row, bold marks the cursor (so it stands out among marked
    // rows); on a normal row, bold conveys the file type as before.
    let bold = if highlighted { selected } else { type_bold };
    if bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }

    // A single leading space as a gutter (the former *-marker column).
    let left = format!(" {}", name);
    queue!(out, Print(&left))?;

    let used = util::display_width(&left);
    if used + info_w < width {
        let pad = width - used - info_w;
        queue!(out, Print(" ".repeat(pad)), Print(&info))?;
    } else if used < width {
        queue!(out, Print(" ".repeat(width - used)))?;
    }

    // Clear attributes and restore the theme base so nothing bleeds into later cells.
    queue!(out, SetAttribute(Attribute::Reset), SetForegroundColor(t.fg), SetBackgroundColor(t.bg))?;
    Ok(())
}

fn entry_style(entry: &Entry, t: &Theme) -> (Color, bool) {
    if !entry.accessible || (entry.is_link && !entry.link_ok) {
        return (t.broken, false);
    }
    match entry.ftype {
        FType::Dir => (t.dir, true),
        FType::Symlink => (t.link, false),
        FType::Fifo | FType::Socket => (t.special, false),
        FType::BlockDevice | FType::CharDevice => (t.device, true),
        FType::File if entry.executable => (t.exec, true),
        _ => (t.fg, false),
    }
}

/// Render a byte count according to the configured size format.
fn format_size(bytes: u64, settings: &Settings) -> String {
    match settings.size_format {
        SizeFormat::Human => util::human_size(bytes),
        SizeFormat::Binary => util::human_size_binary(bytes),
        SizeFormat::Bytes => bytes.to_string(),
    }
}

/// The right-aligned info block for a row: the date column (when `with_time`)
/// followed by the size, both in fixed-width fields so they align as columns.
/// Directories show no size.
fn info_string(entry: &Entry, settings: &Settings, with_time: bool) -> String {
    let size = match entry.ftype {
        FType::Dir => String::new(),
        _ => format_size(entry.size, settings),
    };
    if !with_time {
        return size;
    }
    let secs = match settings.time_type {
        TimeType::Modified => entry.mtime,
        TimeType::Created => entry.created,
        TimeType::Changed => entry.ctime,
        TimeType::Accessed => entry.atime,
    };
    let date = util::format_time(secs, matches!(settings.time_format, TimeFormat::DateTime));
    // Date column, then a fixed-width size column kept flush to the right edge.
    // Raw byte counts need a wider field than the compact human-readable forms.
    let size_w = if matches!(settings.size_format, SizeFormat::Bytes) { 11 } else { 6 };
    format!("{}  {:>size_w$}", date, size)
}

// ---- status bar ------------------------------------------------------------

fn draw_statusbar(out: &mut impl Write, app: &App, y: u16, cols: usize, t: &Theme) -> io::Result<()> {
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
            SetBackgroundColor(t.bg),
            SetForegroundColor(t.progress),
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
            format_size(entry.size, &app.settings),
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
        SetBackgroundColor(t.bg),
        SetForegroundColor(t.info),
        Print(left),
        Print(" ".repeat(pad)),
        SetForegroundColor(t.accent),
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

fn draw_vline(out: &mut impl Write, x: u16, top: u16, height: usize, t: &Theme) -> io::Result<()> {
    queue!(out, SetBackgroundColor(t.bg), SetForegroundColor(t.border))?;
    for row in 0..height {
        queue!(out, MoveTo(x, top + row as u16), Print("│"))?;
    }
    queue!(out, ResetColor)?;
    Ok(())
}

/// Index of the first visible row, chosen to keep the focused row vertically
/// centered in the column, clamped so the list never scrolls past its top or
/// bottom (so early/late entries sit near the top/bottom rather than centered).
fn scroll_begin(pointer: usize, len: usize, height: usize) -> usize {
    if len <= height {
        return 0;
    }
    let max_offset = len - height;
    pointer.saturating_sub(height / 2).min(max_offset)
}

#[cfg(test)]
mod tests {
    use super::scroll_begin;

    #[test]
    fn centers_focused_row_with_room_on_both_sides() {
        // Long list, cursor in the middle: focused row sits at height/2.
        let height = 20;
        let len = 100;
        let pointer = 50;
        let offset = scroll_begin(pointer, len, height);
        assert_eq!(pointer - offset, height / 2);
    }

    #[test]
    fn clamps_at_top_and_bottom() {
        let (len, height) = (100, 20);
        // Near the top: can't center, cursor stays at its real row.
        assert_eq!(scroll_begin(3, len, height), 0);
        // Near the bottom: scrolled to the last page, cursor moves toward bottom.
        assert_eq!(scroll_begin(99, len, height), len - height);
        // Short list: no scrolling at all.
        assert_eq!(scroll_begin(5, 10, height), 0);
    }
}
