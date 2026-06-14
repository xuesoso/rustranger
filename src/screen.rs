//! A minimal cell back-buffer with diff-based flushing (ncurses-style).
//!
//! Drawing builds a grid of styled cells in memory; [`flush`] then emits escape
//! sequences only for the cells that actually changed since the previous frame.
//! A cursor move thus repaints a couple of rows, not the whole screen — which is
//! what keeps rustranger smooth over tmux/SSH. We never clear the screen on a
//! normal frame (a full-screen clear forces the host/tmux to repaint every cell
//! = flicker); [`clear`] is used only once on resize / first paint.

use std::io::{self, Write};

use crossterm::style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate};
use crossterm::{cursor::MoveTo, queue};

use crate::util;

/// Foreground colour + background colour + the two attributes rustranger uses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub reverse: bool,
}

impl Style {
    pub fn new(fg: Color, bg: Color) -> Style {
        Style {
            fg,
            bg,
            bold: false,
            reverse: false,
        }
    }
    pub fn bold(mut self) -> Style {
        self.bold = true;
        self
    }
    pub fn reversed(mut self) -> Style {
        self.reverse = true;
        self
    }
}

#[derive(Clone, PartialEq)]
struct Cell {
    ch: char,
    style: Style,
    /// True for the second column of a wide (2-cell) character; never emitted
    /// on its own — the wide char to its left already advanced the cursor.
    cont: bool,
}

#[derive(Clone)]
pub struct Buffer {
    pub cols: usize,
    pub rows: usize,
    default: Style,
    cells: Vec<Cell>,
}

impl Buffer {
    pub fn new(cols: usize, rows: usize, default: Style) -> Buffer {
        let blank = Cell {
            ch: ' ',
            style: default,
            cont: false,
        };
        Buffer {
            cols,
            rows,
            default,
            cells: vec![blank; cols * rows],
        }
    }

    /// Reset every cell to a blank of `default` (called at the start of a frame).
    pub fn reset(&mut self, default: Style) {
        self.default = default;
        let blank = Cell {
            ch: ' ',
            style: default,
            cont: false,
        };
        for c in self.cells.iter_mut() {
            c.clone_from(&blank);
        }
    }

    fn put(&mut self, x: usize, y: usize, cell: Cell) {
        if x < self.cols && y < self.rows {
            self.cells[y * self.cols + x] = cell;
        }
    }

    /// Write `s` at (x, y) in `style`, clipped to the right edge. Returns the
    /// column just past the written text.
    pub fn set_str(&mut self, x: usize, y: usize, s: &str, style: Style) -> usize {
        if y >= self.rows {
            return x;
        }
        let mut cx = x;
        for ch in s.chars() {
            if cx >= self.cols {
                break;
            }
            let w = util::char_width(ch);
            if w == 0 {
                continue; // skip control / combining marks
            }
            self.put(cx, y, Cell { ch, style, cont: false });
            if w == 2 && cx + 1 < self.cols {
                self.put(
                    cx + 1,
                    y,
                    Cell {
                        ch: ' ',
                        style,
                        cont: true,
                    },
                );
            }
            cx += w;
        }
        cx
    }

    /// Place a single character at (x, y).
    pub fn set_char(&mut self, x: usize, y: usize, ch: char, style: Style) {
        let w = util::char_width(ch).max(1);
        self.put(x, y, Cell { ch, style, cont: false });
        if w == 2 && x + 1 < self.cols {
            self.put(x + 1, y, Cell { ch: ' ', style, cont: true });
        }
    }
}

/// Clear the whole screen to `style`'s background. Used only on first paint /
/// resize — never on a normal frame (see module docs).
pub fn clear(out: &mut impl Write, style: Style) -> io::Result<()> {
    queue!(
        out,
        SetForegroundColor(style.fg),
        SetBackgroundColor(style.bg),
        Clear(ClearType::All),
    )
}

/// Emit the minimal escape sequences to turn the displayed `prev` frame into
/// `cur`. With no `prev` (or a size change) every cell is (re)drawn. Wrapped in
/// synchronized-update markers for terminals that honor them.
pub fn flush(out: &mut impl Write, prev: Option<&Buffer>, cur: &Buffer) -> io::Result<()> {
    queue!(out, BeginSynchronizedUpdate)?;
    let same_size = prev
        .map(|p| p.cols == cur.cols && p.rows == cur.rows)
        .unwrap_or(false);
    let mut active: Option<Style> = None;
    // Where the terminal cursor sits in our model, so contiguous runs of changed
    // cells avoid a redundant MoveTo before every character.
    let mut pen: Option<(usize, usize)> = None;

    for y in 0..cur.rows {
        let mut x = 0;
        while x < cur.cols {
            let i = y * cur.cols + x;
            let cell = &cur.cells[i];
            if cell.cont {
                x += 1;
                continue;
            }
            let w = util::char_width(cell.ch).max(1);
            let changed = !same_size || prev.unwrap().cells[i] != *cell;
            if changed {
                if pen != Some((x, y)) {
                    queue!(out, MoveTo(x as u16, y as u16))?;
                }
                if active != Some(cell.style) {
                    emit_style(out, cell.style)?;
                    active = Some(cell.style);
                }
                queue!(out, Print(cell.ch))?;
                pen = Some((x + w, y));
            }
            x += w;
        }
    }
    queue!(out, ResetColor, EndSynchronizedUpdate)?;
    Ok(())
}

fn emit_style(out: &mut impl Write, s: Style) -> io::Result<()> {
    // Reset first so a previous bold/reverse never lingers, then set the colours
    // and any attributes for this run.
    queue!(
        out,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(s.fg),
        SetBackgroundColor(s.bg),
    )?;
    if s.bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if s.reverse {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    Ok(())
}
