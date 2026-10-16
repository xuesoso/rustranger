//! Following the terminal's light/dark appearance for `theme = "auto"`.
//!
//! Two signals, both answered through the terminal's input stream (decoded by
//! `input`) — no helper processes, no threads:
//!
//! - **`OSC 11` query** — "what is your background color?" The answer is the
//!   ground truth: its luminance picks the light or dark theme. Asked once at
//!   startup, and again on a keystroke at most every [`RECHECK`], so a change
//!   is noticed on the next key press even where nothing announces it. Each
//!   query is followed by `DA1`, whose answer marks the reply complete.
//! - **Mode 2031** — terminals that support it (Ghostty, kitty, and tmux 3.6
//!   panes, among others) push `CSI ? 997 ; 1|2 n` the moment their color scheme
//!   changes. That triggers an immediate re-query, so the switch happens without
//!   waiting for a key.
//!
//! A query costs one small write; the reply is decoded with the keystrokes, so
//! nothing ever waits on it — except the bounded startup probe, which holds the
//! first paint back (at most [`STARTUP_WAIT`]) so it is drawn in the right theme.

use std::time::{Duration, Instant};

use crossterm::style::Color;

/// Ask for the background color (`OSC 11 ; ?`, ST-terminated).
pub const QUERY_BACKGROUND: &[u8] = b"\x1b]11;?\x1b\\";
/// Primary device attributes. Every terminal answers it, and answers in order,
/// so its reply marks the end of the startup query: a terminal that ignores
/// `OSC 11` costs one round trip, not the whole timeout.
pub const QUERY_DEVICE_ATTRIBUTES: &[u8] = b"\x1b[c";
/// Subscribe to / unsubscribe from color-scheme change notifications (mode 2031).
/// Terminals without it ignore both.
pub const NOTIFY_ON: &[u8] = b"\x1b[?2031h";
pub const NOTIFY_OFF: &[u8] = b"\x1b[?2031l";

/// Longest the first paint waits for the startup answer.
pub const STARTUP_WAIT: Duration = Duration::from_millis(200);
/// Longest we hold the terminal back from a child program (or the shell, at
/// quit) for an answer still on its way — otherwise it would arrive as stray
/// "typed" text in the editor or at the prompt.
pub const SETTLE_WAIT: Duration = Duration::from_millis(250);
/// Minimum spacing of keystroke-triggered re-checks: a burst of typing sends one
/// query, while the first key after a pause always sends one. Also how long an
/// unanswered query is considered in flight.
pub const RECHECK: Duration = Duration::from_secs(1);

/// Whether an RGB background is dark — relative luminance below 0.5, the rule
/// folio's `--theme auto` uses, so rustranger and its image previews agree.
pub fn rgb_is_dark(r: u8, g: u8, b: u8) -> bool {
    let chan = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * chan(r) + 0.7152 * chan(g) + 0.0722 * chan(b) < 0.5
}

/// [`rgb_is_dark`] for a theme color. Non-RGB backgrounds (the terminal's own)
/// are passed to folio as `none`, which its `auto` treats as light.
pub fn color_is_dark(bg: Color) -> bool {
    match bg {
        Color::Rgb { r, g, b } => rgb_is_dark(r, g, b),
        _ => false,
    }
}

/// When to query, and whether an answer is still on its way.
#[derive(Default)]
pub struct Tracker {
    /// When the last query was sent.
    last_query: Option<Instant>,
    /// Sent and not yet answered (its `DA1` hasn't come back).
    in_flight: bool,
    /// Whether we have subscribed to mode-2031 notifications.
    pub subscribed: bool,
}

impl Tracker {
    /// Whether a key press at `now` should send a query: at most one per
    /// [`RECHECK`], so a burst of typing sends one and the first key after a
    /// pause always does.
    pub fn key_due(&self, now: Instant) -> bool {
        self.last_query.is_none_or(|t| now.duration_since(t) >= RECHECK)
    }

    /// Whether a scheme-change notification at `now` should send a query: right
    /// away, unless one is already in flight (its answer will do).
    pub fn notification_due(&self, now: Instant) -> bool {
        !self.in_flight(now)
    }

    /// Record a query sent at `now`.
    pub fn sent(&mut self, now: Instant) {
        self.last_query = Some(now);
        self.in_flight = true;
    }

    /// The terminal finished answering (its `DA1` arrived).
    pub fn answered(&mut self) {
        self.in_flight = false;
    }

    /// Whether an answer may still arrive. A terminal that never answers is
    /// given up on after [`RECHECK`], so it can't stall anything for long.
    pub fn in_flight(&self, now: Instant) -> bool {
        self.in_flight && self.last_query.is_some_and(|t| now.duration_since(t) < RECHECK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darkness_matches_folio_auto() {
        assert!(rgb_is_dark(0x28, 0x28, 0x28)); // gruvbox-dark
        assert!(rgb_is_dark(0x1f, 0x1f, 0x28)); // a real tmux/Ghostty answer
        assert!(!rgb_is_dark(0xfb, 0xf1, 0xc7)); // gruvbox-light
        assert!(!color_is_dark(Color::Reset));
    }

    #[test]
    fn keystroke_queries_are_paced() {
        let mut tr = Tracker::default();
        let t = Instant::now();
        assert!(tr.key_due(t), "first key always checks");
        tr.sent(t);
        assert!(!tr.key_due(t + Duration::from_millis(300)), "typing burst: one query");
        assert!(tr.key_due(t + RECHECK), "after a pause it checks again");
    }

    #[test]
    fn in_flight_until_answered_or_given_up() {
        let mut tr = Tracker::default();
        let t = Instant::now();
        assert!(!tr.in_flight(t));
        tr.sent(t);
        assert!(tr.in_flight(t + Duration::from_millis(10)));
        assert!(!tr.notification_due(t), "a query is already on its way");
        tr.answered();
        assert!(!tr.in_flight(t + Duration::from_millis(10)));
        assert!(tr.notification_due(t));
        // A terminal that never answers is not waited on forever.
        tr.sent(t);
        assert!(!tr.in_flight(t + RECHECK));
    }
}
