//! Color themes. A `Theme` maps every semantic UI role to a color; named themes
//! provide common light/dark palettes, and individual roles can be overridden
//! from the config file's `[theme]` section (or `:set`/`--theme` at runtime).

use crossterm::style::Color;

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,       // screen background
    pub fg: Color,       // default text / regular files
    pub border: Color,   // column separators, menu/help frames
    pub title: Color,    // path head, menu titles, help headers (bold)
    pub accent: Color,   // basename, tabs, position counter, menu items
    pub dir: Color,
    pub link: Color,     // symlinks
    pub exec: Color,     // executable files
    pub special: Color,  // fifo / socket
    pub device: Color,   // block / char device
    pub broken: Color,   // inaccessible / broken link
    pub warning: Color,  // "(empty)" markers
    pub error: Color,
    pub info: Color,     // dim placeholders / status-bar metadata
    pub progress: Color, // copy/move progress bar
}

/// Build an RGB color from a 0xRRGGBB literal.
fn rgb(hex: u32) -> Color {
    Color::Rgb {
        r: ((hex >> 16) & 0xff) as u8,
        g: ((hex >> 8) & 0xff) as u8,
        b: (hex & 0xff) as u8,
    }
}

impl Theme {
    /// Look up a built-in theme by name. Returns None for an unknown name.
    pub fn by_name(name: &str) -> Option<Theme> {
        let t = match name.trim().to_ascii_lowercase().as_str() {
            "default" | "terminal" => Theme::terminal(),

            // ---- dark ----
            "gruvbox-dark" | "gruvbox" => Theme {
                bg: rgb(0x282828), fg: rgb(0xebdbb2), border: rgb(0x504945),
                title: rgb(0xfabd2f), accent: rgb(0x83a598), dir: rgb(0x83a598),
                link: rgb(0x8ec07c), exec: rgb(0xb8bb26), special: rgb(0xd3869b),
                device: rgb(0xfe8019), broken: rgb(0xfb4934), warning: rgb(0xfabd2f),
                error: rgb(0xfb4934), info: rgb(0x928374), progress: rgb(0xb8bb26),
            },
            "solarized-dark" => Theme {
                bg: rgb(0x002b36), fg: rgb(0x93a1a1), border: rgb(0x073642),
                title: rgb(0xb58900), accent: rgb(0x268bd2), dir: rgb(0x268bd2),
                link: rgb(0x2aa198), exec: rgb(0x859900), special: rgb(0xd33682),
                device: rgb(0xcb4b16), broken: rgb(0xdc322f), warning: rgb(0xb58900),
                error: rgb(0xdc322f), info: rgb(0x586e75), progress: rgb(0x859900),
            },
            "nord" => Theme {
                bg: rgb(0x2e3440), fg: rgb(0xd8dee9), border: rgb(0x434c5e),
                title: rgb(0x88c0d0), accent: rgb(0x81a1c1), dir: rgb(0x81a1c1),
                link: rgb(0x88c0d0), exec: rgb(0xa3be8c), special: rgb(0xb48ead),
                device: rgb(0xd08770), broken: rgb(0xbf616a), warning: rgb(0xebcb8b),
                error: rgb(0xbf616a), info: rgb(0x616e88), progress: rgb(0xa3be8c),
            },
            "dracula" => Theme {
                bg: rgb(0x282a36), fg: rgb(0xf8f8f2), border: rgb(0x44475a),
                title: rgb(0xbd93f9), accent: rgb(0x8be9fd), dir: rgb(0xbd93f9),
                link: rgb(0x8be9fd), exec: rgb(0x50fa7b), special: rgb(0xff79c6),
                device: rgb(0xffb86c), broken: rgb(0xff5555), warning: rgb(0xf1fa8c),
                error: rgb(0xff5555), info: rgb(0x6272a4), progress: rgb(0x50fa7b),
            },

            // ---- light ----
            "gruvbox-light" => Theme {
                bg: rgb(0xfbf1c7), fg: rgb(0x3c3836), border: rgb(0xd5c4a1),
                title: rgb(0xb57614), accent: rgb(0x076678), dir: rgb(0x076678),
                link: rgb(0x427b58), exec: rgb(0x79740e), special: rgb(0x8f3f71),
                device: rgb(0xaf3a03), broken: rgb(0x9d0006), warning: rgb(0xb57614),
                error: rgb(0x9d0006), info: rgb(0x7c6f64), progress: rgb(0x79740e),
            },
            "solarized-light" => Theme {
                bg: rgb(0xfdf6e3), fg: rgb(0x586e75), border: rgb(0xeee8d5),
                title: rgb(0xb58900), accent: rgb(0x268bd2), dir: rgb(0x268bd2),
                link: rgb(0x2aa198), exec: rgb(0x859900), special: rgb(0xd33682),
                device: rgb(0xcb4b16), broken: rgb(0xdc322f), warning: rgb(0xb58900),
                error: rgb(0xdc322f), info: rgb(0x93a1a1), progress: rgb(0x859900),
            },
            "one-light" => Theme {
                bg: rgb(0xfafafa), fg: rgb(0x383a42), border: rgb(0xc9c9c9),
                title: rgb(0x4078f2), accent: rgb(0x0184bc), dir: rgb(0x4078f2),
                link: rgb(0x0184bc), exec: rgb(0x50a14f), special: rgb(0xa626a4),
                device: rgb(0xc18401), broken: rgb(0xe45649), warning: rgb(0xc18401),
                error: rgb(0xe45649), info: rgb(0xa0a1a7), progress: rgb(0x50a14f),
            },
            "ayu-light" => Theme {
                bg: rgb(0xfcfcfc), fg: rgb(0x5c6166), border: rgb(0xe7e8e9),
                title: rgb(0xfa8d3e), accent: rgb(0x399ee6), dir: rgb(0x399ee6),
                link: rgb(0x4cbf99), exec: rgb(0x86b300), special: rgb(0xa37acc),
                device: rgb(0xf2ae49), broken: rgb(0xf07171), warning: rgb(0xf2ae49),
                error: rgb(0xf07171), info: rgb(0x8a9199), progress: rgb(0x86b300),
            },

            _ => return None,
        };
        Some(t)
    }

    /// The terminal-native theme: keep the terminal's own fg/bg (so nothing is
    /// painted) and use the classic ANSI accent colors — the look before themes.
    pub fn terminal() -> Theme {
        Theme {
            bg: Color::Reset, fg: Color::Reset, border: Color::DarkGrey,
            title: Color::Blue, accent: Color::Cyan, dir: Color::Blue,
            link: Color::Cyan, exec: Color::Green, special: Color::Magenta,
            device: Color::Yellow, broken: Color::Red, warning: Color::Yellow,
            error: Color::Red, info: Color::DarkGrey, progress: Color::Green,
        }
    }

    /// Override a single role from a `[theme]` config entry.
    pub fn set_field(&mut self, key: &str, c: Color) {
        match key {
            "bg" | "background" => self.bg = c,
            "fg" | "foreground" | "text" => self.fg = c,
            "border" => self.border = c,
            "title" | "heading" => self.title = c,
            "accent" => self.accent = c,
            "dir" | "directory" => self.dir = c,
            "link" | "symlink" => self.link = c,
            "exec" | "executable" => self.exec = c,
            "special" => self.special = c,
            "device" => self.device = c,
            "broken" => self.broken = c,
            "warning" | "empty" => self.warning = c,
            "error" => self.error = c,
            "info" | "dim" => self.info = c,
            "progress" => self.progress = c,
            _ => {}
        }
    }

    /// Comma-separated list of built-in theme names (for help/messages).
    pub fn names() -> &'static str {
        "default, gruvbox-dark, gruvbox-light, solarized-dark, solarized-light, \
         nord, dracula, one-light, ayu-light"
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::terminal()
    }
}

/// Parse a color from a config value: `#rrggbb`/`rrggbb`, an ANSI index `0`-`255`,
/// a basic name (`blue`, `darkred`, `grey`, …), or `reset`/`default`.
pub fn parse_color(s: &str) -> Option<Color> {
    let l = s.trim().to_ascii_lowercase();
    let named = match l.as_str() {
        "reset" | "default" | "none" => Some(Color::Reset),
        "black" => Some(Color::Black),
        "darkgrey" | "darkgray" => Some(Color::DarkGrey),
        "grey" | "gray" => Some(Color::Grey),
        "white" => Some(Color::White),
        "red" => Some(Color::Red),
        "darkred" => Some(Color::DarkRed),
        "green" => Some(Color::Green),
        "darkgreen" => Some(Color::DarkGreen),
        "yellow" => Some(Color::Yellow),
        "darkyellow" => Some(Color::DarkYellow),
        "blue" => Some(Color::Blue),
        "darkblue" => Some(Color::DarkBlue),
        "magenta" | "purple" => Some(Color::Magenta),
        "darkmagenta" => Some(Color::DarkMagenta),
        "cyan" => Some(Color::Cyan),
        "darkcyan" => Some(Color::DarkCyan),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let hex = l.strip_prefix('#').unwrap_or(&l);
    if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        if let Ok(v) = u32::from_str_radix(hex, 16) {
            return Some(rgb(v));
        }
    }
    l.parse::<u8>().ok().map(Color::AnsiValue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_themes_resolve_and_unknown_does_not() {
        for name in [
            "default", "gruvbox-dark", "gruvbox-light", "solarized-dark",
            "solarized-light", "nord", "dracula", "one-light", "ayu-light",
        ] {
            assert!(Theme::by_name(name).is_some(), "{name} should resolve");
        }
        assert!(Theme::by_name("bogus").is_none());
        // case/space insensitive
        assert!(Theme::by_name("  Gruvbox-Dark ").is_some());
    }

    #[test]
    fn parse_color_forms() {
        assert!(matches!(parse_color("#ff8800"), Some(Color::Rgb { r: 255, g: 136, b: 0 })));
        assert!(matches!(parse_color("00ff00"), Some(Color::Rgb { r: 0, g: 255, b: 0 })));
        assert!(matches!(parse_color("blue"), Some(Color::Blue)));
        assert!(matches!(parse_color("reset"), Some(Color::Reset)));
        assert!(matches!(parse_color("123"), Some(Color::AnsiValue(123))));
        assert!(parse_color("nope!").is_none());
    }
}
