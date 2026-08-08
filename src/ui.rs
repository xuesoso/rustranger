// Miller-columns renderer (parent | current | preview) with borders and bars.
// Ported in spirit from ranger/gui/widgets/view_miller.py + browsercolumn.py.
//
// Drawing targets an in-memory cell `Buffer` (see crate::screen); the run loop
// diffs successive buffers and emits only changed cells. Colors come from the
// active `Theme`; the buffer is reset to the theme background each frame so the
// whole screen is "painted" without ever clearing it.

use crossterm::style::Color;

use crate::app::App;
use crate::config::{Settings, SizeFormat, TimeFormat, TimeType};
use crate::fs::{Dir, Entry, FType};
use crate::screen::{Buffer, Style};
use crate::theme::Theme;
use crate::util;

/// Minimum display width reserved for a file/folder name before the date/size
/// info column is allowed to take space (the info is dropped in narrower columns).
const MIN_NAME_WIDTH: usize = 24;

/// Render one frame into `buf` (sized to the terminal). Returns the hardware
/// cursor position to show (only in console mode), or None to keep it hidden.
pub fn render(buf: &mut Buffer, app: &App) -> Option<(u16, u16)> {
    let t = &app.settings.theme;
    buf.reset(Style::new(t.fg, t.bg));

    let (cols, rows) = (buf.cols, buf.rows);
    if rows < 3 || cols < 4 {
        return None;
    }

    draw_titlebar(buf, app, cols, t);

    let body_top = 1usize;
    let body_height = rows - 2;
    let mut cursor = None;

    if let Some(scroll) = app.help {
        // The help overlay replaces the browser view entirely while open.
        draw_help(buf, scroll, body_top, body_height, cols, t);
    } else {
        draw_miller(buf, app, body_top, body_height, cols, t);
        if app.console.is_some() {
            cursor = draw_console(buf, app, rows - 1, cols, t);
        } else {
            draw_statusbar(buf, app, rows - 1, cols, t);
        }
        // A pending key-chain hint (e.g. the sort menu) overlays everything else.
        if let Some(menu) = &app.menu {
            draw_menu(buf, menu, cols, rows, t);
        }
    }

    cursor
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

// ---- title bar -------------------------------------------------------------

fn draw_titlebar(buf: &mut Buffer, app: &App, cols: usize, t: &Theme) {
    let base = Style::new(t.fg, t.bg);

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
    let mut x = buf.set_str(0, 0, &head, Style::new(t.title, t.bg).bold());
    if util::display_width(&head) + util::display_width(&tail) <= path_budget {
        x = buf.set_str(x, 0, &tail, base);
    }
    let _ = x;

    if !tabs.is_empty() {
        buf.set_str(cols - tabs_w, 0, &tabs, Style::new(t.accent, t.bg));
    }
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

// ---- key-chain menu --------------------------------------------------------

/// Draw a key-chain hint menu as a bordered popup anchored to the bottom-right,
/// just above the status bar. Long lists are clamped to the available height.
fn draw_menu(buf: &mut Buffer, menu: &crate::app::KeyMenu, cols: usize, rows: usize, t: &Theme) {
    let max_items = rows.saturating_sub(5).max(1);
    let mut items: Vec<(String, String)> = menu.items.clone();
    if items.len() > max_items {
        items.truncate(max_items.saturating_sub(1));
        items.push((String::new(), "…".to_string()));
    }

    let key_w = items.iter().map(|(k, _)| util::display_width(k)).max().unwrap_or(1);
    let interior = items
        .iter()
        .map(|(_, d)| key_w + 2 + util::display_width(d))
        .chain(std::iter::once(util::display_width(&menu.title)))
        .max()
        .unwrap_or(0)
        .min(cols.saturating_sub(5));

    let box_w = interior + 4;
    let box_h = items.len() + 3;
    let x = cols.saturating_sub(box_w + 1);
    let top = rows.saturating_sub(box_h + 1).max(1);
    let avail = cols.saturating_sub(x);

    let border = Style::new(t.border, t.bg);
    let hline = "─".repeat(interior + 2);

    buf.set_str(x, top, &util::truncate(&format!("┌{}┐", hline), avail), border);

    let mut cx = buf.set_str(x, top + 1, "│ ", border);
    cx = buf.set_str(
        cx,
        top + 1,
        &util::truncate(&pad_right(&menu.title, interior), interior),
        Style::new(t.title, t.bg).bold(),
    );
    buf.set_str(cx, top + 1, " │", border);

    for (i, (key, desc)) in items.iter().enumerate() {
        let pad = " ".repeat(key_w.saturating_sub(util::display_width(key)));
        let row = format!("{}{}  {}", pad, key, desc);
        let mut cx = buf.set_str(x, top + 2 + i, "│ ", border);
        cx = buf.set_str(
            cx,
            top + 2 + i,
            &util::truncate(&pad_right(&row, interior), interior),
            Style::new(t.accent, t.bg),
        );
        buf.set_str(cx, top + 2 + i, " │", border);
    }

    buf.set_str(x, top + box_h - 1, &util::truncate(&format!("└{}┘", hline), avail), border);
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
    "  yn / yb           copy name / base name (no ext) to clipboard",
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
    "  zh                toggle hidden files",
    "  zi                toggle image/document preview",
    "  zm                toggle mouse support",
    "  / , n / N         search, next / previous match",
    "  :                 command console (mkdir, touch, chmod, filter, set, …)",
    "  S                 run a shell command",
    "  r                 open selection with a program",
    "  ?                 this help",
    "",
    "Mouse  (zm toggles; hold Shift/Option for terminal text selection)",
    "  wheel             scroll the list — or the preview, over that pane",
    "  left click        select and enter/open (like l)",
    "  right click       go to the parent directory (like h)",
];

/// Number of help lines (used by the key handler to clamp scrolling).
pub fn help_len() -> usize {
    HELP.len()
}

/// Draw the scrollable help overlay as a full-width bordered panel.
fn draw_help(buf: &mut Buffer, scroll: usize, top: usize, height: usize, cols: usize, t: &Theme) {
    let border = Style::new(t.border, t.bg);
    let inner_w = cols.saturating_sub(4);
    let inner_h = height.saturating_sub(2);
    let max_scroll = HELP.len().saturating_sub(inner_h);
    let scroll = scroll.min(max_scroll);

    let mut tb = String::from("┌─ Help (j/k scroll · g/G top/bottom · q close) ");
    let used = util::display_width(&tb);
    if used + 1 < cols {
        tb.push_str(&"─".repeat(cols - 1 - used));
    }
    tb.push('┐');
    buf.set_str(0, top, &util::truncate(&tb, cols), border);

    for row in 0..inner_h {
        let y = top + 1 + row;
        let mut cx = buf.set_str(0, y, "│ ", border);
        if let Some(line) = HELP.get(scroll + row) {
            let header = !line.is_empty() && !line.starts_with(' ');
            let style = if header {
                Style::new(t.title, t.bg).bold()
            } else {
                Style::new(t.fg, t.bg)
            };
            cx = buf.set_str(cx, y, &pad_right(&util::truncate(line, inner_w), inner_w), style);
        } else {
            cx = buf.set_str(cx, y, &" ".repeat(inner_w), Style::new(t.fg, t.bg));
        }
        buf.set_str(cx, y, " │", border);
    }

    buf.set_str(
        0,
        top + height - 1,
        &util::truncate(&format!("└{}┘", "─".repeat(cols.saturating_sub(2))), cols),
        border,
    );
}

// ---- console ---------------------------------------------------------------

fn draw_console(buf: &mut Buffer, app: &App, y: usize, cols: usize, t: &Theme) -> Option<(u16, u16)> {
    let console = app.console.as_ref()?;
    let line = format!("{}{}", console.prompt, console.input);
    buf.set_str(0, y, &util::truncate(&line, cols), Style::new(t.fg, t.bg));
    let cursor_x = (1 + console.cursor).min(cols.saturating_sub(1));
    Some((cursor_x as u16, y as u16))
}

// ---- miller layout ---------------------------------------------------------

fn draw_miller(buf: &mut Buffer, app: &App, top: usize, height: usize, cols: usize, t: &Theme) {
    let ratios = &app.settings.column_ratios;
    let layout = column_layout(cols, ratios);
    let n = layout.len();

    for (i, &(x, width)) in layout.iter().enumerate() {
        let is_preview = i + 1 == n && n > 1;
        let is_main = i + 1 == n - 1 || (n == 1);

        if is_preview {
            draw_preview_column(buf, app, x, top, width, height, t);
        } else if is_main {
            draw_filelist(
                buf,
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
            );
        } else if let Some(p) = app.parent_path().and_then(|p| app.get_cached(&p).map(|_| p)) {
            let dir = app.get_cached(&p).unwrap();
            draw_filelist(buf, dir, &app.tags, &app.settings, false, x, top, width, height, true, t);
        }

        // Separator/border to the right of this column.
        if i + 1 < n && app.settings.draw_borders {
            draw_vline(buf, x + width, top, height, t);
        }
    }
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
    let assigned: usize = widths.iter().sum();
    if usable > assigned {
        let idx = n.saturating_sub(2);
        widths[idx] += usable - assigned;
    }

    let mut out = Vec::with_capacity(n);
    let mut x = 0usize;
    for w in widths {
        out.push((x, w));
        x += w + 1;
    }
    out
}

/// What sits at a given screen cell — the inverse of [`draw_miller`]'s layout,
/// used to route mouse clicks and wheel events to the right pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    /// An entry in the parent column, by index into that directory's visible list.
    Parent(usize),
    /// An entry in the current (main) column, by index into its visible list.
    Current(usize),
    /// Anywhere inside the preview pane.
    Preview,
    /// Chrome or dead space: title bar, status bar, a column separator, or a row
    /// past the end of a short listing.
    Nothing,
}

/// Map a terminal cell `(x, y)` to what is drawn there. Mirrors `render` +
/// `draw_miller` + `draw_filelist` exactly — including the scroll offset — so a
/// click lands on the row the user actually sees.
pub fn hit_test(app: &App, cols: usize, rows: usize, x: u16, y: u16) -> Hit {
    if rows < 3 || cols < 4 {
        return Hit::Nothing;
    }
    let (x, y) = (x as usize, y as usize);

    // Row 0 is the title bar and the last row is the status/console bar.
    let (body_top, body_height) = (1usize, rows - 2);
    if y < body_top || y >= body_top + body_height {
        return Hit::Nothing;
    }

    let layout = column_layout(cols, &app.settings.column_ratios);
    let n = layout.len();
    // A click in the 1-cell gap/border between panes belongs to neither.
    let Some(i) = layout.iter().position(|&(cx, w)| x >= cx && x < cx + w) else {
        return Hit::Nothing;
    };

    let is_preview = i + 1 == n && n > 1;
    let is_main = i + 1 == n - 1 || n == 1;
    if is_preview {
        return Hit::Preview;
    }

    // Non-preview, non-main columns all show the parent directory (as drawn).
    let dir = if is_main {
        Some(app.current_dir())
    } else {
        app.parent_path().and_then(|p| app.get_cached(&p))
    };
    let Some(dir) = dir else { return Hit::Nothing };
    // An error or "(empty)" placeholder occupies the column instead of a list.
    if dir.error.is_some() || dir.is_empty() {
        return Hit::Nothing;
    }

    let idx = scroll_begin(dir.pointer, dir.len(), body_height) + (y - body_top);
    if idx >= dir.len() {
        return Hit::Nothing; // blank rows below a short listing
    }
    if is_main {
        Hit::Current(idx)
    } else {
        Hit::Parent(idx)
    }
}

/// The preview column's cell rectangle `(x, top, width, height)`, matching what
/// `draw_miller` uses — for placing a graphics image over the preview pane. None
/// when the screen is too small or the layout has no preview column.
pub fn preview_rect(cols: usize, rows: usize, ratios: &[u32]) -> Option<(u16, u16, u16, u16)> {
    if rows < 3 || cols < 4 {
        return None;
    }
    let layout = column_layout(cols, ratios);
    let n = layout.len();
    if n < 2 {
        return None; // no preview column (single-column layout)
    }
    let (x, width) = layout[n - 1];
    if width == 0 {
        return None;
    }
    Some((x as u16, 1, width as u16, (rows - 2) as u16))
}

fn draw_preview_column(buf: &mut Buffer, app: &App, x: usize, top: usize, width: usize, height: usize, t: &Theme) {
    let Some(entry) = app.current_dir().current() else {
        return;
    };
    if entry.is_dir() && entry.accessible {
        if let Some(dir) = app.get_cached(&entry.path) {
            draw_filelist(buf, dir, &app.tags, &app.settings, false, x, top, width, height, false, t);
        }
    } else if matches!(entry.ftype, FType::File) {
        // Image/document previews are painted as terminal graphics over this box
        // after the cell flush (see imgpreview) — leave the cells blank so text
        // doesn't fight the image.
        if !app.is_image_preview(&entry.path) {
            draw_file_preview(buf, app, x, top, width, height, t);
        }
    } else {
        let info = format!("({})", entry.ftype.name());
        buf.set_str(x, top, &util::truncate(&info, width), Style::new(t.info, t.bg));
    }
}

fn draw_file_preview(buf: &mut Buffer, app: &App, x: usize, top: usize, width: usize, height: usize, t: &Theme) {
    use crate::preview::Preview;
    match app.current_preview() {
        Some(Preview::Text(lines)) => {
            let start = app.preview_scroll.min(lines.len().saturating_sub(1));
            let style = Style::new(t.fg, t.bg);
            for (row, line) in lines.iter().skip(start).take(height).enumerate() {
                buf.set_str(x, top + row, &util::truncate(line, width), style);
            }
        }
        Some(Preview::Binary) => placeholder(buf, x, top, width, "(binary file)", t.info, t),
        Some(Preview::TooBig) => placeholder(buf, x, top, width, "(file too large to preview)", t.info, t),
        Some(Preview::Empty) => placeholder(buf, x, top, width, "(empty)", t.warning, t),
        Some(Preview::Error(e)) => placeholder(buf, x, top, width, &format!("(error: {})", e), t.error, t),
        None => {}
    }
}

fn placeholder(buf: &mut Buffer, x: usize, top: usize, width: usize, msg: &str, color: Color, t: &Theme) {
    buf.set_str(x, top, &util::truncate(msg, width), Style::new(color, t.bg));
}

// ---- file list column ------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn draw_filelist(
    buf: &mut Buffer,
    dir: &Dir,
    tags: &crate::state::tags::Tags,
    settings: &Settings,
    with_time: bool,
    x: usize,
    top: usize,
    width: usize,
    height: usize,
    focused: bool,
    t: &Theme,
) {
    if let Some(err) = &dir.error {
        buf.set_str(x, top, &util::truncate(&format!("error: {}", err), width), Style::new(t.error, t.bg));
        return;
    }
    if dir.is_empty() {
        buf.set_str(x, top, &util::truncate("(empty)", width), Style::new(t.warning, t.bg));
        return;
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
        draw_entry_row(buf, entry, tag, selected, settings, with_time, x, top + row, width, t);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_entry_row(
    buf: &mut Buffer,
    entry: &Entry,
    tag: Option<char>,
    selected: bool,
    settings: &Settings,
    with_time: bool,
    x: usize,
    y: usize,
    width: usize,
    t: &Theme,
) {
    let (color, type_bold) = entry_style(entry, t);
    // Marked (spacebar) and tagged (t) entries, and the cursor row, invert.
    let marked = entry.marked || tag.is_some();
    let highlighted = selected || marked;

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

    // Build the full-width row so the whole column cell range carries the row's
    // style (so a highlighted row is a solid bar to the right edge).
    let left = format!(" {}", name);
    let used = util::display_width(&left);
    let mut row = left;
    if used + info_w < width {
        row.push_str(&" ".repeat(width - used - info_w));
        row.push_str(&info);
    } else if used < width {
        row.push_str(&" ".repeat(width - used));
    }

    let mut style = Style::new(color, t.bg);
    if highlighted {
        style = style.reversed();
    }
    // On an inverted row, bold marks the cursor among marked rows; otherwise bold
    // conveys the file type.
    let bold = if highlighted { selected } else { type_bold };
    if bold {
        style = style.bold();
    }
    buf.set_str(x, y, &row, style);
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
    let size_w = if matches!(settings.size_format, SizeFormat::Bytes) { 11 } else { 6 };
    format!("{}  {:>size_w$}", date, size)
}

// ---- status bar ------------------------------------------------------------

fn draw_statusbar(buf: &mut Buffer, app: &App, y: usize, cols: usize, t: &Theme) {
    let dir = app.current_dir();

    // While a background copy/move runs, show an aggregate progress bar instead.
    if app.jobs_active() {
        let (done, total) = app.jobs.iter().fold((0u64, 0u64), |(d, tot), j| {
            (d + j.progress().done, tot + j.progress().total)
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
        buf.set_str(0, y, &util::truncate(&text, cols), Style::new(t.progress, t.bg));
        return;
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

    let info = Style::new(t.info, t.bg);
    let mut cx = buf.set_str(0, y, &left, info);
    cx = buf.set_str(cx, y, &" ".repeat(pad), info);
    buf.set_str(cx, y, &right, Style::new(t.accent, t.bg));
}

fn target_of(entry: &Entry) -> String {
    std::fs::read_link(&entry.path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---- helpers ---------------------------------------------------------------

fn draw_vline(buf: &mut Buffer, x: usize, top: usize, height: usize, t: &Theme) {
    let style = Style::new(t.border, t.bg);
    for row in 0..height {
        buf.set_char(x, top + row, '│', style);
    }
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
    use super::{render, scroll_begin};
    use crate::app::App;
    use crate::config::Settings;
    use crate::screen::{flush, Buffer, Style};
    use crossterm::style::Color;

    /// Rendering then flushing must emit a synchronized-update frame containing
    /// the directory content, with no screen/line clears (the flicker fix), and
    /// must no-op on a terminal too small to draw.
    #[test]
    fn render_then_flush_emits_clean_frame_with_content() {
        let dir = std::env::temp_dir().join(format!("rr_render_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), b"hi there").unwrap();

        let mut app = App::new(dir.clone(), Settings::default());
        app.prepare_view();

        let mut buf = Buffer::new(80, 24, Style::new(Color::Reset, Color::Reset));
        let _cursor = render(&mut buf, &app);

        let mut out: Vec<u8> = Vec::new();
        flush(&mut out, None, &buf).unwrap();
        let s = String::from_utf8_lossy(&out);

        let begin = s.find("\u{1b}[?2026h").expect("begin synchronized update");
        let end = s.rfind("\u{1b}[?2026l").expect("end synchronized update");
        assert!(begin < end);
        assert!(s.contains("hello.txt"), "directory content rendered");
        // The flicker fix: a frame must never clear the screen or a line.
        assert!(!s.contains("\u{1b}[2J"), "must not clear the whole screen");
        assert!(!s.contains("\u{1b}[2K"), "must not clear a line");
        assert!(!s.contains("\u{1b}[K"), "must not clear to end of line");

        // A terminal too small to draw renders no cursor and panics on nothing.
        let mut tiny = Buffer::new(2, 2, Style::new(Color::Reset, Color::Reset));
        assert!(render(&mut tiny, &app).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Mouse hit-testing must invert the miller layout exactly: the right pane,
    /// the right entry, and nothing at all for chrome or dead space.
    #[test]
    fn hit_test_maps_cells_to_panes_and_rows() {
        use super::{hit_test, Hit};

        // base/sub/{file_00..file_99}, opened at `sub` so the parent column is
        // deterministic: it lists exactly one entry ("sub").
        let base = std::env::temp_dir().join(format!("rr_hit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        for i in 0..100 {
            std::fs::write(sub.join(format!("file_{:02}", i)), b"x").unwrap();
        }
        let mut app = App::new(sub.clone(), Settings::default());
        app.prepare_view();

        // Default ratios 1:3:4 at 80 cols → parent x0..8, main x10..39, preview x41..
        let (cols, rows) = (80usize, 24usize);

        // Chrome is never a target.
        assert_eq!(hit_test(&app, cols, rows, 20, 0), Hit::Nothing, "title bar");
        let last = rows as u16 - 1;
        assert_eq!(hit_test(&app, cols, rows, 20, last), Hit::Nothing, "status bar");
        assert_eq!(hit_test(&app, cols, rows, 9, 1), Hit::Nothing, "column separator");

        // Main column: the first body row is the first visible entry.
        assert_eq!(hit_test(&app, cols, rows, 20, 1), Hit::Current(0));
        assert_eq!(hit_test(&app, cols, rows, 20, 5), Hit::Current(4));

        // Preview pane, and the parent column's single entry.
        assert_eq!(hit_test(&app, cols, rows, 60, 3), Hit::Preview);
        assert_eq!(hit_test(&app, cols, rows, 3, 1), Hit::Parent(0));
        assert_eq!(hit_test(&app, cols, rows, 3, 2), Hit::Nothing, "past end of parent");

        // Scrolling shifts which entry a given cell refers to — the same offset
        // the renderer uses, so clicks track what the user sees.
        app.move_cursor(50);
        let offset = scroll_begin(50, 100, rows - 2);
        assert!(offset > 0, "list is scrolled");
        assert_eq!(hit_test(&app, cols, rows, 20, 1), Hit::Current(offset));
        assert_eq!(hit_test(&app, cols, rows, 20, 4), Hit::Current(offset + 3));

        // A terminal too small to draw has no targets.
        assert_eq!(hit_test(&app, 2, 2, 0, 0), Hit::Nothing);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn diff_flush_emits_only_changed_cells() {
        // Two near-identical frames differing in one cell -> the diff must emit
        // far less than a full-screen repaint.
        let base = Style::new(Color::White, Color::Black);
        let mut a = Buffer::new(80, 24, base);
        a.set_str(0, 0, "hello world", base);
        let mut b = Buffer::new(80, 24, base);
        b.set_str(0, 0, "hello world", base);
        b.set_str(0, 5, "X", Style::new(Color::Red, Color::Black));

        let mut out: Vec<u8> = Vec::new();
        flush(&mut out, Some(&a), &b).unwrap();
        // Only the single changed cell (plus sync wrap + one move + style) is sent.
        assert!(out.len() < 64, "incremental diff should be tiny, got {}", out.len());
        assert!(String::from_utf8_lossy(&out).contains('X'));
    }

    #[test]
    fn centers_focused_row_with_room_on_both_sides() {
        let height = 20;
        let len = 100;
        let pointer = 50;
        let offset = scroll_begin(pointer, len, height);
        assert_eq!(pointer - offset, height / 2);
    }

    #[test]
    fn clamps_at_top_and_bottom() {
        let (len, height) = (100, 20);
        assert_eq!(scroll_begin(3, len, height), 0);
        assert_eq!(scroll_begin(99, len, height), len - height);
        assert_eq!(scroll_begin(5, 10, height), 0);
    }
}
