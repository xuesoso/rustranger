//! Terminal input: raw bytes from the tty in, key/mouse events and terminal
//! replies out.
//!
//! rustranger reads the terminal itself instead of through crossterm's event
//! reader, because it needs to see the terminal's *answers* — the `OSC 11`
//! background-color reply and mode-2031 color-scheme notifications that drive
//! `theme = "auto"`. crossterm's reader has no slot for them: an `OSC 11` reply
//! is decoded as Alt+`]` followed by a stream of ordinary keystrokes (which
//! would run as commands), and a `CSI ? 997 ; 1 n` notification leaves its
//! parser waiting for a final byte it will never accept, swallowing the keys
//! typed after it.
//!
//! The key/mouse decoding is a port of crossterm 0.28.1's Unix parser
//! (`src/event/sys/unix/parse.rs`, MIT License, Copyright (c) 2019 Timon), so
//! keys decode exactly as they did before. Changes from the original: replies
//! to our queries are returned as [`Input`] variants, every `CSI ?` sequence
//! ends at its final byte, and raw mode is assumed (it always is while we read).

use std::collections::VecDeque;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode,
    ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};

/// One decoded unit of terminal input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A key press, mouse action, paste, or focus change.
    Event(Event),
    /// `OSC 11` reply: the terminal's background color.
    Background(u8, u8, u8),
    /// Mode-2031 notification: the terminal switched color scheme (`true` = dark).
    SchemeChanged(bool),
    /// Primary device attributes (`DA1`) — sent after a query so we know when
    /// the terminal has finished answering it.
    DeviceAttributes,
    /// Any other terminal reply (cursor position, mode reports, other OSCs).
    Reply,
}

/// Longest sequence kept while waiting for its end. A reply that never
/// terminates (or garbage) is dropped instead of growing without bound.
const MAX_SEQUENCE: usize = 4096;

/// Incremental decoder: feed it whatever `read(2)` returned, then drain it.
#[derive(Default)]
pub struct Parser {
    buffer: Vec<u8>,
    queue: VecDeque<Input>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode `bytes`. `more` says whether further input is already known to be
    /// waiting — it decides whether a lone `ESC` is the Esc key or the start of
    /// a sequence still arriving (crossterm passes "the read filled its buffer").
    pub fn advance(&mut self, bytes: &[u8], more: bool) {
        for (idx, &byte) in bytes.iter().enumerate() {
            let more = idx + 1 < bytes.len() || more;
            self.buffer.push(byte);
            match parse_event(&self.buffer, more) {
                Ok(Some(input)) => {
                    self.queue.push_back(input);
                    self.buffer.clear();
                }
                // Incomplete: keep the bytes for the next round, within reason.
                Ok(None) if self.buffer.len() < MAX_SEQUENCE => {}
                Ok(None) | Err(()) => self.buffer.clear(),
            }
        }
    }

    pub fn next(&mut self) -> Option<Input> {
        self.queue.pop_front()
    }

    /// Whether decoded input is waiting to be taken.
    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Look at the decoded input without taking it.
    pub fn pending(&self) -> impl Iterator<Item = &Input> {
        self.queue.iter()
    }
}

type Parsed = Result<Option<Input>, ()>;

fn key(code: KeyCode) -> Parsed {
    Ok(Some(Input::Event(Event::Key(code.into()))))
}

fn key_with(code: KeyCode, modifiers: KeyModifiers) -> Parsed {
    Ok(Some(Input::Event(Event::Key(KeyEvent::new(code, modifiers)))))
}

fn parse_event(buffer: &[u8], input_available: bool) -> Parsed {
    if buffer.is_empty() {
        return Ok(None);
    }
    match buffer[0] {
        b'\x1B' => {
            if buffer.len() == 1 {
                if input_available {
                    // Possible Esc sequence
                    Ok(None)
                } else {
                    key(KeyCode::Esc)
                }
            } else {
                match buffer[1] {
                    b'O' => {
                        if buffer.len() == 2 {
                            Ok(None)
                        } else {
                            match buffer[2] {
                                b'D' => key(KeyCode::Left),
                                b'C' => key(KeyCode::Right),
                                b'A' => key(KeyCode::Up),
                                b'B' => key(KeyCode::Down),
                                b'H' => key(KeyCode::Home),
                                b'F' => key(KeyCode::End),
                                // F1-F4
                                val @ b'P'..=b'S' => key(KeyCode::F(1 + val - b'P')),
                                _ => Err(()),
                            }
                        }
                    }
                    b'[' => parse_csi(buffer),
                    // An OSC reply — unless nothing follows the `]`, in which
                    // case it was Alt+] typed by hand.
                    b']' if buffer.len() > 2 || input_available => parse_osc(buffer),
                    b'\x1B' => key(KeyCode::Esc),
                    _ => parse_event(&buffer[1..], input_available).map(|input| {
                        input.map(|input| match input {
                            Input::Event(Event::Key(mut key_event)) => {
                                key_event.modifiers |= KeyModifiers::ALT;
                                Input::Event(Event::Key(key_event))
                            }
                            other => other,
                        })
                    }),
                }
            }
        }
        b'\r' => key(KeyCode::Enter),
        b'\t' => key(KeyCode::Tab),
        b'\x7F' => key(KeyCode::Backspace),
        // In raw mode `\n` is Ctrl+J (crossterm issue #371), so it falls in here.
        c @ b'\x01'..=b'\x1A' => key_with(KeyCode::Char((c - 0x1 + b'a') as char), KeyModifiers::CONTROL),
        c @ b'\x1C'..=b'\x1F' => key_with(KeyCode::Char((c - 0x1C + b'4') as char), KeyModifiers::CONTROL),
        b'\0' => key_with(KeyCode::Char(' '), KeyModifiers::CONTROL),
        _ => parse_utf8_char(buffer)
            .map(|maybe_char| maybe_char.map(|c| Input::Event(Event::Key(char_code_to_event(KeyCode::Char(c)))))),
    }
}

// converts KeyCode to KeyEvent (adds shift modifier in case of uppercase characters)
fn char_code_to_event(code: KeyCode) -> KeyEvent {
    let modifiers = match code {
        KeyCode::Char(c) if c.is_uppercase() => KeyModifiers::SHIFT,
        _ => KeyModifiers::empty(),
    };
    KeyEvent::new(code, modifiers)
}

/// `ESC ] Ps ; Pt` terminated by BEL or ST (`ESC \`).
fn parse_osc(buffer: &[u8]) -> Parsed {
    let body = &buffer[2..];
    // Every reply starts with its numeric code (`11;…`).
    if body.first().is_some_and(|c| !c.is_ascii_digit()) {
        return Err(());
    }
    let text = match body {
        [rest @ .., b'\x07'] => rest,
        [rest @ .., b'\x1B', b'\\'] => rest,
        // An ESC inside the body can only begin the ST terminator.
        [.., b'\x1B'] => return Ok(None),
        [.., b'\x1B', _] => return Err(()),
        // Replies are printable text; a control byte means this isn't one.
        [.., c] if *c < 0x20 => return Err(()),
        _ => return Ok(None),
    };
    let text = std::str::from_utf8(text).map_err(|_| ())?;
    Ok(Some(match text.strip_prefix("11;").and_then(parse_osc_color) {
        Some((r, g, b)) => Input::Background(r, g, b),
        None => Input::Reply,
    }))
}

/// Parse an X11 color spec as terminals report it: `rgb:R/G/B` (or `rgba:R/G/B/A`)
/// with 1-4 hex digits per channel, scaled to 8 bits.
pub fn parse_osc_color(spec: &str) -> Option<(u8, u8, u8)> {
    let channels = spec.strip_prefix("rgb:").or_else(|| spec.strip_prefix("rgba:"))?;
    let mut it = channels.split('/').map(|c| {
        if c.is_empty() || c.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(c, 16).ok()?;
        let max = (1u32 << (4 * c.len())) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    });
    Some((it.next()??, it.next()??, it.next()??))
}

fn parse_csi(buffer: &[u8]) -> Parsed {
    assert!(buffer.starts_with(b"\x1B[")); // ESC [

    if buffer.len() == 2 {
        return Ok(None);
    }

    let input_event = match buffer[2] {
        b'[' => {
            if buffer.len() == 3 {
                None
            } else {
                match buffer[3] {
                    // NOTE (@imdaveho): cannot find when this occurs;
                    // having another '[' after ESC[ not a likely scenario
                    val @ b'A'..=b'E' => Some(Event::Key(KeyCode::F(1 + val - b'A').into())),
                    _ => return Err(()),
                }
            }
        }
        b'D' => Some(Event::Key(KeyCode::Left.into())),
        b'C' => Some(Event::Key(KeyCode::Right.into())),
        b'A' => Some(Event::Key(KeyCode::Up.into())),
        b'B' => Some(Event::Key(KeyCode::Down.into())),
        b'H' => Some(Event::Key(KeyCode::Home.into())),
        b'F' => Some(Event::Key(KeyCode::End.into())),
        b'Z' => Some(Event::Key(KeyEvent::new_with_kind(
            KeyCode::BackTab,
            KeyModifiers::SHIFT,
            KeyEventKind::Press,
        ))),
        b'M' => return parse_csi_normal_mouse(buffer),
        b'<' => return parse_csi_sgr_mouse(buffer),
        b'I' => Some(Event::FocusGained),
        b'O' => Some(Event::FocusLost),
        b';' => return parse_csi_modifier_key_code(buffer),
        // P, Q, and S for compatibility with Kitty keyboard protocol,
        // as the 1 in 'CSI 1 P' etc. must be omitted if there are no
        // modifiers pressed:
        // https://sw.kovidgoyal.net/kitty/keyboard-protocol/#legacy-functional-keys
        b'P' => Some(Event::Key(KeyCode::F(1).into())),
        b'Q' => Some(Event::Key(KeyCode::F(2).into())),
        b'S' => Some(Event::Key(KeyCode::F(4).into())),
        b'?' => return parse_csi_private_reply(buffer),
        b'0'..=b'9' => {
            // Numbered escape code.
            if buffer.len() == 3 {
                None
            } else {
                // The final byte of a CSI sequence can be in the range 64-126, so
                // let's keep reading anything else.
                let last_byte = buffer[buffer.len() - 1];
                if !(64..=126).contains(&last_byte) {
                    None
                } else {
                    if buffer.starts_with(b"\x1B[200~") {
                        return parse_csi_bracketed_paste(buffer);
                    }
                    match last_byte {
                        b'M' => return parse_csi_rxvt_mouse(buffer),
                        b'~' => return parse_csi_special_key_code(buffer),
                        b'u' => return parse_csi_u_encoded_key_code(buffer),
                        // Cursor position report: nothing we asked for.
                        b'R' => return Ok(Some(Input::Reply)),
                        _ => return parse_csi_modifier_key_code(buffer),
                    }
                }
            }
        }
        _ => return Err(()),
    };

    Ok(input_event.map(Input::Event))
}

/// `CSI ? … final` — always a terminal reply: device attributes (`c`), a
/// color-scheme notification (`997 ; 1|2 n`), keyboard flags (`u`), a mode
/// report (`$ y`), … The sequence runs to its final byte (0x40-0x7E); crossterm
/// waited for a `u` or `c` only, eating every key typed after any other reply.
fn parse_csi_private_reply(buffer: &[u8]) -> Parsed {
    let last = buffer[buffer.len() - 1];
    if buffer.len() == 3 || !(0x40..=0x7E).contains(&last) {
        return Ok(None);
    }
    let params = &buffer[3..buffer.len() - 1];
    Ok(Some(match last {
        b'c' => Input::DeviceAttributes,
        b'n' if params == b"997;1" => Input::SchemeChanged(true),
        b'n' if params == b"997;2" => Input::SchemeChanged(false),
        _ => Input::Reply,
    }))
}

fn next_parsed<T>(iter: &mut dyn Iterator<Item = &str>) -> Result<T, ()>
where
    T: std::str::FromStr,
{
    iter.next().ok_or(())?.parse::<T>().map_err(|_| ())
}

fn modifier_and_kind_parsed(iter: &mut dyn Iterator<Item = &str>) -> Result<(u8, u8), ()> {
    let mut sub_split = iter.next().ok_or(())?.split(':');

    let modifier_mask = next_parsed::<u8>(&mut sub_split)?;

    if let Ok(kind_code) = next_parsed::<u8>(&mut sub_split) {
        Ok((modifier_mask, kind_code))
    } else {
        Ok((modifier_mask, 1))
    }
}

fn parse_modifiers(mask: u8) -> KeyModifiers {
    let modifier_mask = mask.saturating_sub(1);
    let mut modifiers = KeyModifiers::empty();
    if modifier_mask & 1 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if modifier_mask & 2 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if modifier_mask & 4 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    if modifier_mask & 8 != 0 {
        modifiers |= KeyModifiers::SUPER;
    }
    if modifier_mask & 16 != 0 {
        modifiers |= KeyModifiers::HYPER;
    }
    if modifier_mask & 32 != 0 {
        modifiers |= KeyModifiers::META;
    }
    modifiers
}

fn parse_modifiers_to_state(mask: u8) -> KeyEventState {
    let modifier_mask = mask.saturating_sub(1);
    let mut state = KeyEventState::empty();
    if modifier_mask & 64 != 0 {
        state |= KeyEventState::CAPS_LOCK;
    }
    if modifier_mask & 128 != 0 {
        state |= KeyEventState::NUM_LOCK;
    }
    state
}

fn parse_key_event_kind(kind: u8) -> KeyEventKind {
    match kind {
        1 => KeyEventKind::Press,
        2 => KeyEventKind::Repeat,
        3 => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    }
}

fn parse_csi_modifier_key_code(buffer: &[u8]) -> Parsed {
    assert!(buffer.starts_with(b"\x1B[")); // ESC [
    let s = std::str::from_utf8(&buffer[2..buffer.len() - 1]).map_err(|_| ())?;
    let mut split = s.split(';');

    split.next();

    let (modifiers, kind) = if let Ok((modifier_mask, kind_code)) = modifier_and_kind_parsed(&mut split) {
        (parse_modifiers(modifier_mask), parse_key_event_kind(kind_code))
    } else if buffer.len() > 3 {
        (
            parse_modifiers((buffer[buffer.len() - 2] as char).to_digit(10).ok_or(())? as u8),
            KeyEventKind::Press,
        )
    } else {
        (KeyModifiers::NONE, KeyEventKind::Press)
    };
    let key = buffer[buffer.len() - 1];

    let keycode = match key {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'F' => KeyCode::End,
        b'H' => KeyCode::Home,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return Err(()),
    };

    Ok(Some(Input::Event(Event::Key(KeyEvent::new_with_kind(keycode, modifiers, kind)))))
}

fn translate_functional_key_code(codepoint: u32) -> Option<(KeyCode, KeyEventState)> {
    if let Some(keycode) = match codepoint {
        57399 => Some(KeyCode::Char('0')),
        57400 => Some(KeyCode::Char('1')),
        57401 => Some(KeyCode::Char('2')),
        57402 => Some(KeyCode::Char('3')),
        57403 => Some(KeyCode::Char('4')),
        57404 => Some(KeyCode::Char('5')),
        57405 => Some(KeyCode::Char('6')),
        57406 => Some(KeyCode::Char('7')),
        57407 => Some(KeyCode::Char('8')),
        57408 => Some(KeyCode::Char('9')),
        57409 => Some(KeyCode::Char('.')),
        57410 => Some(KeyCode::Char('/')),
        57411 => Some(KeyCode::Char('*')),
        57412 => Some(KeyCode::Char('-')),
        57413 => Some(KeyCode::Char('+')),
        57414 => Some(KeyCode::Enter),
        57415 => Some(KeyCode::Char('=')),
        57416 => Some(KeyCode::Char(',')),
        57417 => Some(KeyCode::Left),
        57418 => Some(KeyCode::Right),
        57419 => Some(KeyCode::Up),
        57420 => Some(KeyCode::Down),
        57421 => Some(KeyCode::PageUp),
        57422 => Some(KeyCode::PageDown),
        57423 => Some(KeyCode::Home),
        57424 => Some(KeyCode::End),
        57425 => Some(KeyCode::Insert),
        57426 => Some(KeyCode::Delete),
        57427 => Some(KeyCode::KeypadBegin),
        _ => None,
    } {
        return Some((keycode, KeyEventState::KEYPAD));
    }

    if let Some(keycode) = match codepoint {
        57358 => Some(KeyCode::CapsLock),
        57359 => Some(KeyCode::ScrollLock),
        57360 => Some(KeyCode::NumLock),
        57361 => Some(KeyCode::PrintScreen),
        57362 => Some(KeyCode::Pause),
        57363 => Some(KeyCode::Menu),
        v @ 57376..=57398 => Some(KeyCode::F((v - 57376 + 13) as u8)),
        57428 => Some(KeyCode::Media(MediaKeyCode::Play)),
        57429 => Some(KeyCode::Media(MediaKeyCode::Pause)),
        57430 => Some(KeyCode::Media(MediaKeyCode::PlayPause)),
        57431 => Some(KeyCode::Media(MediaKeyCode::Reverse)),
        57432 => Some(KeyCode::Media(MediaKeyCode::Stop)),
        57433 => Some(KeyCode::Media(MediaKeyCode::FastForward)),
        57434 => Some(KeyCode::Media(MediaKeyCode::Rewind)),
        57435 => Some(KeyCode::Media(MediaKeyCode::TrackNext)),
        57436 => Some(KeyCode::Media(MediaKeyCode::TrackPrevious)),
        57437 => Some(KeyCode::Media(MediaKeyCode::Record)),
        57438 => Some(KeyCode::Media(MediaKeyCode::LowerVolume)),
        57439 => Some(KeyCode::Media(MediaKeyCode::RaiseVolume)),
        57440 => Some(KeyCode::Media(MediaKeyCode::MuteVolume)),
        57441 => Some(KeyCode::Modifier(ModifierKeyCode::LeftShift)),
        57442 => Some(KeyCode::Modifier(ModifierKeyCode::LeftControl)),
        57443 => Some(KeyCode::Modifier(ModifierKeyCode::LeftAlt)),
        57444 => Some(KeyCode::Modifier(ModifierKeyCode::LeftSuper)),
        57445 => Some(KeyCode::Modifier(ModifierKeyCode::LeftHyper)),
        57446 => Some(KeyCode::Modifier(ModifierKeyCode::LeftMeta)),
        57447 => Some(KeyCode::Modifier(ModifierKeyCode::RightShift)),
        57448 => Some(KeyCode::Modifier(ModifierKeyCode::RightControl)),
        57449 => Some(KeyCode::Modifier(ModifierKeyCode::RightAlt)),
        57450 => Some(KeyCode::Modifier(ModifierKeyCode::RightSuper)),
        57451 => Some(KeyCode::Modifier(ModifierKeyCode::RightHyper)),
        57452 => Some(KeyCode::Modifier(ModifierKeyCode::RightMeta)),
        57453 => Some(KeyCode::Modifier(ModifierKeyCode::IsoLevel3Shift)),
        57454 => Some(KeyCode::Modifier(ModifierKeyCode::IsoLevel5Shift)),
        _ => None,
    } {
        return Some((keycode, KeyEventState::empty()));
    }

    None
}

fn parse_csi_u_encoded_key_code(buffer: &[u8]) -> Parsed {
    assert!(buffer.starts_with(b"\x1B[")); // ESC [
    assert!(buffer.ends_with(b"u"));

    // `CSI unicode-key-code:alternate-key-codes ; modifiers:event-type ; text u`
    // (fixterms `CSI u` / the kitty keyboard protocol).
    let s = std::str::from_utf8(&buffer[2..buffer.len() - 1]).map_err(|_| ())?;
    let mut split = s.split(';');

    let mut codepoints = split.next().ok_or(())?.split(':');

    let codepoint = codepoints.next().ok_or(())?.parse::<u32>().map_err(|_| ())?;

    let (mut modifiers, kind, state_from_modifiers) =
        if let Ok((modifier_mask, kind_code)) = modifier_and_kind_parsed(&mut split) {
            (
                parse_modifiers(modifier_mask),
                parse_key_event_kind(kind_code),
                parse_modifiers_to_state(modifier_mask),
            )
        } else {
            (KeyModifiers::NONE, KeyEventKind::Press, KeyEventState::NONE)
        };

    let (mut keycode, state_from_keycode) = {
        if let Some((special_key_code, state)) = translate_functional_key_code(codepoint) {
            (special_key_code, state)
        } else if let Some(c) = char::from_u32(codepoint) {
            (
                match c {
                    '\x1B' => KeyCode::Esc,
                    '\r' => KeyCode::Enter,
                    '\t' => {
                        if modifiers.contains(KeyModifiers::SHIFT) {
                            KeyCode::BackTab
                        } else {
                            KeyCode::Tab
                        }
                    }
                    '\x7F' => KeyCode::Backspace,
                    _ => KeyCode::Char(c),
                },
                KeyEventState::empty(),
            )
        } else {
            return Err(());
        }
    };

    if let KeyCode::Modifier(modifier_keycode) = keycode {
        match modifier_keycode {
            ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt => modifiers.set(KeyModifiers::ALT, true),
            ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl => {
                modifiers.set(KeyModifiers::CONTROL, true)
            }
            ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift => modifiers.set(KeyModifiers::SHIFT, true),
            ModifierKeyCode::LeftSuper | ModifierKeyCode::RightSuper => modifiers.set(KeyModifiers::SUPER, true),
            ModifierKeyCode::LeftHyper | ModifierKeyCode::RightHyper => modifiers.set(KeyModifiers::HYPER, true),
            ModifierKeyCode::LeftMeta | ModifierKeyCode::RightMeta => modifiers.set(KeyModifiers::META, true),
            _ => {}
        }
    }

    // With "report alternate keys", a shifted key carries the shifted character
    // (per the keyboard layout) as a second ':'-separated codepoint.
    if modifiers.contains(KeyModifiers::SHIFT) {
        if let Some(shifted_c) = codepoints
            .next()
            .and_then(|codepoint| codepoint.parse::<u32>().ok())
            .and_then(char::from_u32)
        {
            keycode = KeyCode::Char(shifted_c);
            modifiers.set(KeyModifiers::SHIFT, false);
        }
    }

    Ok(Some(Input::Event(Event::Key(KeyEvent::new_with_kind_and_state(
        keycode,
        modifiers,
        kind,
        state_from_keycode | state_from_modifiers,
    )))))
}

fn parse_csi_special_key_code(buffer: &[u8]) -> Parsed {
    assert!(buffer.starts_with(b"\x1B[")); // ESC [
    assert!(buffer.ends_with(b"~"));

    let s = std::str::from_utf8(&buffer[2..buffer.len() - 1]).map_err(|_| ())?;
    let mut split = s.split(';');

    // This CSI sequence can be a list of semicolon-separated numbers.
    let first = next_parsed::<u8>(&mut split)?;

    let (modifiers, kind, state) = if let Ok((modifier_mask, kind_code)) = modifier_and_kind_parsed(&mut split) {
        (
            parse_modifiers(modifier_mask),
            parse_key_event_kind(kind_code),
            parse_modifiers_to_state(modifier_mask),
        )
    } else {
        (KeyModifiers::NONE, KeyEventKind::Press, KeyEventState::NONE)
    };

    let keycode = match first {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        v @ 11..=15 => KeyCode::F(v - 10),
        v @ 17..=21 => KeyCode::F(v - 11),
        v @ 23..=26 => KeyCode::F(v - 12),
        v @ 28..=29 => KeyCode::F(v - 15),
        v @ 31..=34 => KeyCode::F(v - 17),
        _ => return Err(()),
    };

    Ok(Some(Input::Event(Event::Key(KeyEvent::new_with_kind_and_state(
        keycode, modifiers, kind, state,
    )))))
}

fn mouse(kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) -> Parsed {
    Ok(Some(Input::Event(Event::Mouse(MouseEvent { kind, column, row, modifiers }))))
}

fn parse_csi_rxvt_mouse(buffer: &[u8]) -> Parsed {
    // rxvt mouse encoding: ESC [ Cb ; Cx ; Cy ; M
    let s = std::str::from_utf8(&buffer[2..buffer.len() - 1]).map_err(|_| ())?;
    let mut split = s.split(';');

    let cb = next_parsed::<u8>(&mut split)?.checked_sub(32).ok_or(())?;
    let (kind, modifiers) = parse_cb(cb)?;

    let cx = next_parsed::<u16>(&mut split)?.checked_sub(1).ok_or(())?;
    let cy = next_parsed::<u16>(&mut split)?.checked_sub(1).ok_or(())?;

    mouse(kind, cx, cy, modifiers)
}

fn parse_csi_normal_mouse(buffer: &[u8]) -> Parsed {
    // Normal mouse encoding: ESC [ M CB Cx Cy (6 characters only).
    if buffer.len() < 6 {
        return Ok(None);
    }

    let cb = buffer[3].checked_sub(32).ok_or(())?;
    let (kind, modifiers) = parse_cb(cb)?;

    // The upper left character position on the terminal is denoted as 1,1.
    // Subtract 1 to keep it synced with cursor
    let cx = u16::from(buffer[4].saturating_sub(32)).saturating_sub(1);
    let cy = u16::from(buffer[5].saturating_sub(32)).saturating_sub(1);

    mouse(kind, cx, cy, modifiers)
}

fn parse_csi_sgr_mouse(buffer: &[u8]) -> Parsed {
    // ESC [ < Cb ; Cx ; Cy (;) (M or m)
    if !buffer.ends_with(b"m") && !buffer.ends_with(b"M") {
        return Ok(None);
    }

    let s = std::str::from_utf8(&buffer[3..buffer.len() - 1]).map_err(|_| ())?;
    let mut split = s.split(';');

    let cb = next_parsed::<u8>(&mut split)?;
    let (kind, modifiers) = parse_cb(cb)?;

    let cx = next_parsed::<u16>(&mut split)?.checked_sub(1).ok_or(())?;
    let cy = next_parsed::<u16>(&mut split)?.checked_sub(1).ok_or(())?;

    // SGR mode tells releases apart by ending them with a lowercase `m`.
    let kind = if buffer.last() == Some(&b'm') {
        match kind {
            MouseEventKind::Down(button) => MouseEventKind::Up(button),
            other => other,
        }
    } else {
        kind
    };

    mouse(kind, cx, cy, modifiers)
}

/// Cb is the byte of a mouse input that contains the button being used, the key modifiers being
/// held and whether the mouse is dragging or not.
///
/// Bit layout of cb, from low to high:
///
/// - button number
/// - button number
/// - shift
/// - meta (alt)
/// - control
/// - mouse is dragging
/// - button number
/// - button number
fn parse_cb(cb: u8) -> Result<(MouseEventKind, KeyModifiers), ()> {
    let button_number = (cb & 0b0000_0011) | ((cb & 0b1100_0000) >> 4);
    let dragging = cb & 0b0010_0000 == 0b0010_0000;

    let kind = match (button_number, dragging) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3, true) | (4, true) | (5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        // We do not support other buttons.
        _ => return Err(()),
    };

    let mut modifiers = KeyModifiers::empty();

    if cb & 0b0000_0100 == 0b0000_0100 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if cb & 0b0000_1000 == 0b0000_1000 {
        modifiers |= KeyModifiers::ALT;
    }
    if cb & 0b0001_0000 == 0b0001_0000 {
        modifiers |= KeyModifiers::CONTROL;
    }

    Ok((kind, modifiers))
}

fn parse_csi_bracketed_paste(buffer: &[u8]) -> Parsed {
    // ESC [ 2 0 0 ~ pasted text ESC 2 0 1 ~
    if !buffer.ends_with(b"\x1b[201~") {
        Ok(None)
    } else {
        let paste = String::from_utf8_lossy(&buffer[6..buffer.len() - 6]).to_string();
        Ok(Some(Input::Event(Event::Paste(paste))))
    }
}

fn parse_utf8_char(buffer: &[u8]) -> Result<Option<char>, ()> {
    match std::str::from_utf8(buffer) {
        Ok(s) => Ok(Some(s.chars().next().ok_or(())?)),
        Err(_) => {
            // from_utf8 failed, but we have to check if we need more bytes for code point
            // and if all the bytes we have no are valid
            let required_bytes = match buffer[0] {
                // https://en.wikipedia.org/wiki/UTF-8#Description
                (0x00..=0x7F) => 1, // 0xxxxxxx
                (0xC0..=0xDF) => 2, // 110xxxxx 10xxxxxx
                (0xE0..=0xEF) => 3, // 1110xxxx 10xxxxxx 10xxxxxx
                (0xF0..=0xF7) => 4, // 11110xxx 10xxxxxx 10xxxxxx 10xxxxxx
                (0x80..=0xBF) | (0xF8..=0xFF) => return Err(()),
            };

            // More than 1 byte, check them for 10xxxxxx pattern
            if required_bytes > 1 && buffer.len() > 1 {
                for byte in &buffer[1..] {
                    if byte & !0b0011_1111 != 0b1000_0000 {
                        return Err(());
                    }
                }
            }

            if buffer.len() < required_bytes {
                // All bytes looks good so far, but we need more of them
                Ok(None)
            } else {
                Err(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(bytes: &[u8]) -> Vec<Input> {
        let mut p = Parser::new();
        p.advance(bytes, false);
        std::iter::from_fn(|| p.next()).collect()
    }

    fn k(code: KeyCode, m: KeyModifiers) -> Input {
        Input::Event(Event::Key(KeyEvent::new(code, m)))
    }

    #[test]
    fn plain_and_control_keys() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            all(b"jK\r\t\x7f\x04\n"),
            vec![
                k(KeyCode::Char('j'), none),
                k(KeyCode::Char('K'), KeyModifiers::SHIFT),
                k(KeyCode::Enter, none),
                k(KeyCode::Tab, none),
                k(KeyCode::Backspace, none),
                k(KeyCode::Char('d'), KeyModifiers::CONTROL),
                k(KeyCode::Char('j'), KeyModifiers::CONTROL), // raw-mode \n
            ]
        );
        assert_eq!(all("é".as_bytes()), vec![k(KeyCode::Char('é'), none)]);
    }

    #[test]
    fn escape_alt_and_arrows() {
        let none = KeyModifiers::NONE;
        assert_eq!(all(b"\x1b"), vec![k(KeyCode::Esc, none)]);
        assert_eq!(all(b"\x1b1"), vec![k(KeyCode::Char('1'), KeyModifiers::ALT)]);
        assert_eq!(all(b"\x1b]"), vec![k(KeyCode::Char(']'), KeyModifiers::ALT)]);
        assert_eq!(
            all(b"\x1b[A\x1bOB\x1b[1;5C\x1b[5~\x1b[Z"),
            vec![
                k(KeyCode::Up, none),
                k(KeyCode::Down, none),
                k(KeyCode::Right, KeyModifiers::CONTROL),
                k(KeyCode::PageUp, none),
                Input::Event(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::BackTab,
                    KeyModifiers::SHIFT,
                    KeyEventKind::Press
                ))),
            ]
        );
    }

    #[test]
    fn sgr_mouse() {
        assert_eq!(
            all(b"\x1b[<0;20;10M\x1b[<65;1;1M"),
            vec![
                Input::Event(Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 19,
                    row: 9,
                    modifiers: KeyModifiers::NONE,
                })),
                Input::Event(Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: 0,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                })),
            ]
        );
    }

    /// The exact reply burst tmux 3.6b sent a probe under Ghostty: background
    /// (ST-terminated), a DECRQM mode report, DA1, then the 2031 notification.
    /// None of it may come out as keystrokes.
    #[test]
    fn terminal_replies_are_not_keys() {
        assert_eq!(
            all(b"\x1b]11;rgb:1f1f/1f1f/2828\x1b\\\x1b[?2031;2$y\x1b[?1;2;4c\x1b[?997;1n"),
            vec![
                Input::Background(0x1f, 0x1f, 0x28),
                Input::Reply,
                Input::DeviceAttributes,
                Input::SchemeChanged(true),
            ]
        );
        assert_eq!(all(b"\x1b]11;rgb:ff/fb/f0\x07"), vec![Input::Background(0xff, 0xfb, 0xf0)]);
        assert_eq!(all(b"\x1b[?997;2n"), vec![Input::SchemeChanged(false)]);
    }

    /// Keys typed around a reply survive — and a reply split across reads (a
    /// slow ssh link) still assembles.
    #[test]
    fn keys_around_a_split_reply_survive() {
        let mut p = Parser::new();
        p.advance(b"j\x1b]11;rgb:28", true);
        p.advance(b"28/2828/2828\x1b", true);
        p.advance(b"\\k\x1b[?997;", true);
        p.advance(b"2nl", false);
        let got: Vec<Input> = std::iter::from_fn(|| p.next()).collect();
        let none = KeyModifiers::NONE;
        assert_eq!(
            got,
            vec![
                k(KeyCode::Char('j'), none),
                Input::Background(0x28, 0x28, 0x28),
                k(KeyCode::Char('k'), none),
                Input::SchemeChanged(false),
                k(KeyCode::Char('l'), none),
            ]
        );
    }

    /// Junk that only looks like the start of a reply is dropped, never
    /// buffered forever, and decoding resumes after it.
    #[test]
    fn malformed_and_runaway_sequences_are_dropped() {
        assert_eq!(all(b"\x1b]11;\x01j"), vec![k(KeyCode::Char('j'), KeyModifiers::NONE)]);
        let mut p = Parser::new();
        let mut junk = b"\x1b]11;".to_vec();
        junk.extend(std::iter::repeat_n(b'a', MAX_SEQUENCE * 2));
        p.advance(&junk, true);
        assert!(p.buffer.len() < MAX_SEQUENCE);
    }

    #[test]
    fn osc_color_specs_scale_to_8_bits() {
        assert_eq!(parse_osc_color("rgb:ffff/0000/8080"), Some((255, 0, 128)));
        assert_eq!(parse_osc_color("rgb:f/0/8"), Some((255, 0, 136)));
        assert_eq!(parse_osc_color("rgba:28/28/28/ff"), Some((0x28, 0x28, 0x28)));
        assert_eq!(parse_osc_color("#282828"), None);
        assert_eq!(parse_osc_color("rgb:12345/0/0"), None);
    }
}
