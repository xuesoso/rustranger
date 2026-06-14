//! Image-preview backends — extension point (scaffold, no backends yet).
//!
//! ranger renders image previews through one of several terminal protocols
//! (w3m, kitty, sixel, iterm2, ueberzug, terminology, urxvt), selected by the
//! `preview_images_method` setting — see ranger/ext/img_display.py.
//!
//! rustranger deliberately ships *without* image preview to stay
//! dependency-free, but this module defines the trait and registry so a backend
//! can be added later without disturbing the rest of the UI. To add one:
//!
//!   1. implement [`ImageDisplay`] for the protocol,
//!   2. return it from [`backend_for`],
//!   3. have the preview pane call [`backend_for`] for image files and forward
//!      a cell rectangle to [`ImageDisplay::draw`] / [`ImageDisplay::clear`].
//!
//! Everything here is currently inert; nothing calls into it.

#![allow(dead_code)]

use std::io;
use std::path::Path;

/// A terminal image-display protocol, mirroring ranger's `preview_images_method`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protocol {
    W3m,
    Kitty,
    Sixel,
    Iterm2,
    Ueberzug,
    Terminology,
    Urxvt,
}

impl Protocol {
    pub fn from_str(s: &str) -> Option<Protocol> {
        Some(match s {
            "w3m" => Protocol::W3m,
            "kitty" => Protocol::Kitty,
            "sixel" => Protocol::Sixel,
            "iterm2" => Protocol::Iterm2,
            "ueberzug" => Protocol::Ueberzug,
            "terminology" => Protocol::Terminology,
            "urxvt" => Protocol::Urxvt,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Protocol::W3m => "w3m",
            Protocol::Kitty => "kitty",
            Protocol::Sixel => "sixel",
            Protocol::Iterm2 => "iterm2",
            Protocol::Ueberzug => "ueberzug",
            Protocol::Terminology => "terminology",
            Protocol::Urxvt => "urxvt",
        }
    }
}

/// A rectangle on the terminal grid, in character cells, where an image should
/// be drawn (typically the preview column).
#[derive(Clone, Copy, Debug)]
pub struct ImageRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// A backend that can draw and clear an image within a cell rectangle.
///
/// Implementations own whatever protocol state they need (escape-sequence
/// writers, child processes such as ueberzug, etc.).
pub trait ImageDisplay {
    /// The protocol this backend implements.
    fn protocol(&self) -> Protocol;

    /// Draw `path` scaled into `rect`.
    fn draw(&mut self, path: &Path, rect: ImageRect) -> io::Result<()>;

    /// Clear any image previously drawn within `rect`.
    fn clear(&mut self, rect: ImageRect) -> io::Result<()>;

    /// Tear down backend resources (e.g. kill a helper process) on exit.
    fn quit(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Construct a backend for `protocol`.
///
/// Returns `None` for every protocol today — no backends are implemented yet.
/// This is the single place future backends get wired in.
pub fn backend_for(protocol: Protocol) -> Option<Box<dyn ImageDisplay>> {
    match protocol {
        Protocol::W3m
        | Protocol::Kitty
        | Protocol::Sixel
        | Protocol::Iterm2
        | Protocol::Ueberzug
        | Protocol::Terminology
        | Protocol::Urxvt => None,
    }
}
