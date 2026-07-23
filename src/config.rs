// Settings that affect browsing. Hardcoded defaults mirror ranger's rc.conf.
// Phase 7 adds optional TOML overrides on top of these defaults.

use crate::fs::sort::SortKey;
use crate::theme::Theme;

/// Which timestamp the file-list date column shows. Covers both the macOS notion
/// of a creation/birth time and the Linux/Unix convention where `ctime` is the
/// inode *change* time (not creation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimeType {
    Modified,
    Created,
    Changed,
    Accessed,
}

impl TimeType {
    pub fn from_str(s: &str) -> Option<TimeType> {
        match s {
            "modified" | "mtime" | "modification" => Some(TimeType::Modified),
            "created" | "creation" | "birth" | "btime" => Some(TimeType::Created),
            // Linux/Unix convention: ctime is the inode status-change time.
            "changed" | "change" | "ctime" | "status" => Some(TimeType::Changed),
            "accessed" | "access" | "atime" => Some(TimeType::Accessed),
            _ => None,
        }
    }
}

/// How file sizes are rendered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SizeFormat {
    /// Human-readable, decimal/SI (1000-based): 1.5k, 1.05M.
    Human,
    /// Human-readable, binary/IEC (1024-based): 1.5K, 1.05M.
    Binary,
    /// Raw byte count.
    Bytes,
}

impl SizeFormat {
    pub fn from_str(s: &str) -> Option<SizeFormat> {
        match s {
            "human" | "si" | "decimal" => Some(SizeFormat::Human),
            "binary" | "iec" | "1024" => Some(SizeFormat::Binary),
            "bytes" | "raw" | "plain" => Some(SizeFormat::Bytes),
            _ => None,
        }
    }
}

/// How the date column is rendered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimeFormat {
    /// YYYY/MM/DD
    Date,
    /// YYYY/MM/DD/HH/MM
    DateTime,
}

impl TimeFormat {
    pub fn from_str(s: &str) -> Option<TimeFormat> {
        match s {
            "date" | "ymd" | "yyyy/mm/dd" => Some(TimeFormat::Date),
            "datetime" | "full" | "yyyy/mm/dd/hh/mm" => Some(TimeFormat::DateTime),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Settings {
    pub show_hidden: bool,
    pub sort: SortKey,
    pub sort_reverse: bool,
    pub sort_directories_first: bool,
    pub sort_case_insensitive: bool,
    pub column_ratios: Vec<u32>,
    pub preview_files: bool,
    pub preview_directories: bool,
    /// Whether image/document files get a terminal-graphics preview. When off,
    /// they fall back to the text preview. Toggled at runtime (ranger's `zi`)
    /// and persisted back to the config file.
    pub preview_images: bool,
    pub draw_borders: bool,
    /// Show the date column (next to size) in the current column's file list.
    pub show_date: bool,
    /// Which timestamp the date column shows (modified or created).
    pub time_type: TimeType,
    /// Date column layout (date only, or date + time).
    pub time_format: TimeFormat,
    /// How file sizes are rendered (human decimal/binary, or raw bytes).
    pub size_format: SizeFormat,
    /// Active color theme (resolved palette; see `[theme]` for per-role overrides).
    pub theme: Theme,
    pub confirm_on_delete: bool,
    pub wrap_scroll: bool,
    /// Names matching this are treated as hidden (in addition to dotfiles).
    pub hidden_filter_dotfiles: bool,
    /// Per-extension open commands from the `[open]` config section: pairs of
    /// (lowercased extension without dot, command template). Looked up when
    /// opening a file before the built-in `$EDITOR`/`xdg-open` fallback.
    pub openers: Vec<(String, String)>,
    /// Per-extension image/document preview commands from the `[preview]` section:
    /// pairs of (lowercased extension, command template). A matching file is
    /// previewed as terminal graphics in the preview pane instead of as text.
    pub preview_cmds: Vec<(String, String)>,
    /// Terminal graphics protocol used for image previews.
    pub preview_protocol: PreviewProtocol,
}

/// Terminal graphics protocol for image previews.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PreviewProtocol {
    /// Pick by terminal: kitty on Ghostty/kitty/WezTerm, iTerm2's inline-images
    /// protocol on iTerm2, sixel elsewhere.
    Auto,
    Kitty,
    Sixel,
    /// iTerm2 OSC 1337 inline images (iTerm2 does not render kitty graphics,
    /// and its sixel support doesn't survive tmux's image re-encoding).
    Iterm,
}

impl PreviewProtocol {
    pub fn from_str(s: &str) -> Option<PreviewProtocol> {
        match s {
            "auto" => Some(PreviewProtocol::Auto),
            "kitty" => Some(PreviewProtocol::Kitty),
            "sixel" => Some(PreviewProtocol::Sixel),
            "iterm" | "iterm2" => Some(PreviewProtocol::Iterm),
            _ => None,
        }
    }
}

impl Settings {
    /// Load settings, applying overrides from an optional config file on top of
    /// the defaults. Looks at $XDG_CONFIG_HOME/rustranger/config.toml (or
    /// ~/.config/rustranger/config.toml).
    pub fn load() -> Settings {
        let mut s = Settings::default();
        if let Some(path) = config_path() {
            if let Ok(text) = std::fs::read_to_string(path) {
                s.apply_toml(&text);
            }
        }
        s
    }

    /// Apply a flat, minimal subset of TOML: `key = value` lines under `[settings]`
    /// and per-role color overrides under `[theme]`. Avoids a TOML crate for a flat
    /// schema. Two passes so a `[theme]` section can override the named `theme`
    /// regardless of section ordering in the file.
    fn apply_toml(&mut self, text: &str) {
        // Pass 1: resolve the base theme so [theme] overrides layer on top of it.
        for (section, key, value) in toml_entries(text) {
            if section == "settings" && key == "theme" {
                if let Some(t) = Theme::by_name(value) {
                    self.theme = t;
                }
            }
        }
        // Pass 2: all settings (the theme name is already applied) + [theme] roles
        // + [open] per-extension open commands.
        for (section, key, value) in toml_entries(text) {
            match section {
                "settings" if key != "theme" => self.set_field(key, value),
                "theme" => {
                    if let Some(c) = crate::theme::parse_color(value) {
                        self.theme.set_field(key, c);
                    }
                }
                "open" => self.set_opener(key, value),
                "preview" => self.set_preview(key, value),
                _ => {}
            }
        }
    }

    /// Register a per-extension open command (from the `[open]` section). The
    /// extension is normalized to lowercase without a leading dot, so both
    /// `csv = ...` and `.CSV = ...` map the same files. A later entry for the
    /// same extension replaces an earlier one.
    pub fn set_opener(&mut self, ext: &str, cmd: &str) {
        let ext = ext.trim().trim_start_matches('.').to_lowercase();
        let cmd = cmd.trim().to_string();
        if ext.is_empty() || cmd.is_empty() {
            return;
        }
        if let Some(slot) = self.openers.iter_mut().find(|(e, _)| *e == ext) {
            slot.1 = cmd;
        } else {
            self.openers.push((ext, cmd));
        }
    }

    /// Register a per-extension preview command (from the `[preview]` section),
    /// normalized like [`set_opener`](Self::set_opener).
    pub fn set_preview(&mut self, ext: &str, cmd: &str) {
        let ext = ext.trim().trim_start_matches('.').to_lowercase();
        let cmd = cmd.trim().to_string();
        if ext.is_empty() || cmd.is_empty() {
            return;
        }
        if let Some(slot) = self.preview_cmds.iter_mut().find(|(e, _)| *e == ext) {
            slot.1 = cmd;
        } else {
            self.preview_cmds.push((ext, cmd));
        }
    }

    /// Apply a single `key value` setting override (shared by the TOML parser and
    /// the command-line `--set key=value` flags). Unknown keys are ignored.
    pub fn set_field(&mut self, key: &str, value: &str) {
        let as_bool = |v: &str| matches!(v, "true" | "1" | "yes" | "on");
        match key {
            "show_hidden" => self.show_hidden = as_bool(value),
            "sort" => {
                if let Some(k) = SortKey::from_str(value) {
                    self.sort = k;
                }
            }
            "sort_reverse" => self.sort_reverse = as_bool(value),
            "sort_directories_first" => self.sort_directories_first = as_bool(value),
            "sort_case_insensitive" => self.sort_case_insensitive = as_bool(value),
            "column_ratios" => {
                let ratios: Vec<u32> = value
                    .split(',')
                    .filter_map(|p| p.trim().parse().ok())
                    .collect();
                if !ratios.is_empty() {
                    self.column_ratios = ratios;
                }
            }
            "preview_files" => self.preview_files = as_bool(value),
            "preview_directories" => self.preview_directories = as_bool(value),
            "preview_images" => self.preview_images = as_bool(value),
            "draw_borders" => self.draw_borders = as_bool(value),
            "show_date" | "show_time" => self.show_date = as_bool(value),
            "time_type" => {
                if let Some(t) = TimeType::from_str(value) {
                    self.time_type = t;
                }
            }
            "time_format" => {
                if let Some(f) = TimeFormat::from_str(value) {
                    self.time_format = f;
                }
            }
            "size_format" => {
                if let Some(f) = SizeFormat::from_str(value) {
                    self.size_format = f;
                }
            }
            "theme" => {
                if let Some(t) = Theme::by_name(value) {
                    self.theme = t;
                }
            }
            "preview_protocol" => {
                if let Some(p) = PreviewProtocol::from_str(value) {
                    self.preview_protocol = p;
                }
            }
            "confirm_on_delete" => self.confirm_on_delete = as_bool(value),
            "wrap_scroll" => self.wrap_scroll = as_bool(value),
            _ => {}
        }
    }
}

/// Strip a trailing `#` comment, ignoring `#` inside double quotes (so hex color
/// values like `"#ff0000"` survive).
fn strip_comment(line: &str) -> &str {
    let mut in_quote = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            '#' if !in_quote => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Iterate `(section, key, value)` triples from a flat TOML body. `section` is one
/// of "settings", "theme", "open", or "" (unknown); comments (`#`) and blanks are
/// skipped.
fn toml_entries(text: &str) -> impl Iterator<Item = (&'static str, &str, &str)> {
    let mut section: &'static str = "";
    text.lines().filter_map(move |raw| {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            return None;
        }
        if line.starts_with('[') {
            section = match line {
                "[settings]" => "settings",
                "[theme]" => "theme",
                "[open]" => "open",
                "[preview]" => "preview",
                _ => "",
            };
            return None;
        }
        let (key, value) = line.split_once('=')?;
        Some((section, key.trim(), value.trim().trim_matches('"')))
    })
}

/// Path to the config file: `$XDG_CONFIG_HOME/rustranger/config.toml`, or
/// `$HOME/.config/rustranger/config.toml`. None if neither var is set.
pub fn config_path() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let base = if let Some(d) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(d)
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".config")
    };
    Some(base.join("rustranger").join("config.toml"))
}

/// An annotated config.toml with every setting at its built-in default.
pub const DEFAULT_CONFIG: &str = "\
# rustranger configuration
# Location: $XDG_CONFIG_HOME/rustranger/config.toml (or ~/.config/rustranger/config.toml)
# Every key is optional; the values below are the built-in defaults.

[settings]
show_hidden = false
sort = \"natural\"               # natural|basename|size|mtime|ctime|atime|type|extension|random
sort_reverse = false
sort_directories_first = true
sort_case_insensitive = true
column_ratios = \"1,3,4\"         # parent : current : preview
preview_files = true
preview_images = true          # terminal-graphics preview for images/PDFs (toggle: zi)
draw_borders = true
confirm_on_delete = true
wrap_scroll = false
show_date = true               # date column next to the size (current column)
time_type = \"modified\"         # modified|created|changed|accessed
time_format = \"date\"           # date (YYYY/MM/DD) | datetime (YYYY/MM/DD/HH/MM)
size_format = \"human\"          # human (1.5k) | binary (1.5K, 1024-based) | bytes (1536)
theme = \"default\"              # default|gruvbox-dark|gruvbox-light|solarized-dark|
                               # solarized-light|nord|dracula|subliminal|gitlab-dark|
                               # gitlab-light|everforest-dark|everforest-light|
                               # one-light|ayu-light

# Override individual theme roles on top of the chosen theme. Values may be
# #rrggbb, a basic color name, an ANSI index 0-255, or \"reset\".
# Roles: bg fg border title accent dir link exec special device broken
#        warning error info progress
# [theme]
# dir = \"#ff8800\"
# accent = \"cyan\"
# bg = \"#11131a\"

# Per-extension open commands (override the built-in $EDITOR / xdg-open default).
# Key = file extension (case-insensitive, no dot). Value = a command run WITHOUT
# a shell (so it works the same on macOS and Linux):
#   - the file path is appended as the last argument, or substituted for `{}`
#   - a trailing `&` runs the program detached (for GUI apps); otherwise it runs
#     in the foreground, suspending the TUI (for terminal apps / pagers / editors)
# [open]
# csv = \"rustidata\"
# tsv = \"rustidata\"
# md  = \"glow -p\"
# zip = \"unzip -l {}\"
# pdf = \"zathura &\"
# html = \"firefox &\"

# In-pane image/document preview: render matching files as terminal graphics in
# the preview pane (needs a kitty-graphics or sixel terminal, e.g. Ghostty/kitty;
# inside tmux set `allow-passthrough on`). The command prints graphics escapes to
# stdout for the given cell box; folio (https://github.com/xuesoso/folio) provides
# `folio print`. Install it and put it on PATH, or replace the command below.
# If the renderer isn't found, previews are simply skipped (no error). Placeholders:
#   %f file  %p protocol  %x col  %y row  %c cols  %r rows
#   %w cell-width-px  %h cell-height-px  %t (expands to --tmux inside tmux)
#   %b theme background as RRGGBB (or \"none\" for terminal-default themes) —
#      folio fills the preview box with it so the box matches the UI theme
# preview_protocol = \"auto\"   # auto (default) | kitty | sixel | iterm
#                              # In tmux, auto asks the tmux server which client is
#                              # attached now: Ghostty/kitty -> kitty, WezTerm ->
#                              # iterm, else (incl. iTerm2) -> tmux-native sixel.
#                              # Outside tmux: kitty on Ghostty/kitty/WezTerm,
#                              # iterm on iTerm2/VSCode, sixel elsewhere.
[preview]
pdf  = \"folio print --protocol %p --col %x --row %y --cols %c --rows %r --cell-width %w --cell-height %h --bg %b %t %f\"
png  = \"folio print --protocol %p --col %x --row %y --cols %c --rows %r --cell-width %w --cell-height %h --bg %b %t %f\"
jpg  = \"folio print --protocol %p --col %x --row %y --cols %c --rows %r --cell-width %w --cell-height %h --bg %b %t %f\"
jpeg = \"folio print --protocol %p --col %x --row %y --cols %c --rows %r --cell-width %w --cell-height %h --bg %b %t %f\"
";

/// Outcome of generating the default config file.
pub enum GenConfig {
    Created(std::path::PathBuf),
    Exists(std::path::PathBuf),
    NoConfigDir,
}

/// Write `DEFAULT_CONFIG` to the config path if it does not already exist,
/// creating parent directories as needed. Never overwrites an existing file.
pub fn generate_default_config() -> std::io::Result<GenConfig> {
    let Some(path) = config_path() else {
        return Ok(GenConfig::NoConfigDir);
    };
    if path.exists() {
        return Ok(GenConfig::Exists(path));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, DEFAULT_CONFIG)?;
    Ok(GenConfig::Created(path))
}

/// Persist a single `[settings]` key back to the on-disk config file so a
/// runtime toggle (e.g. `zi`) survives restarts. Only that one `key = value`
/// line is rewritten — the rest of the user's file, comments and all, is left
/// untouched. If the key is absent it is inserted under `[settings]`; if no
/// config file exists yet, one is seeded from the annotated default template.
pub fn persist_setting(key: &str, value: &str) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory (set HOME or XDG_CONFIG_HOME)",
        )
    })?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_CONFIG.to_string(),
        Err(e) => return Err(e),
    };
    let updated = upsert_setting(&text, key, value);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, updated)
}

/// Return `text` with the `[settings]` entry `key` set to `value`. An existing
/// active *or* commented-out `key = …` line inside `[settings]` is replaced in
/// place (preserving any trailing `# comment`); otherwise the assignment is
/// inserted just below the `[settings]` header, which is created if missing.
fn upsert_setting(text: &str, key: &str, value: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_settings = false;
    let mut settings_hdr: Option<usize> = None;
    let mut done = false;

    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_settings = trimmed == "[settings]";
            if in_settings {
                settings_hdr = Some(out.len());
            }
            out.push(raw.to_string());
            continue;
        }
        // Match an active or commented `key = …` assignment inside [settings].
        if in_settings && !done {
            let body = trimmed.trim_start_matches('#').trim_start();
            if let Some((k, rest)) = body.split_once('=') {
                if k.trim() == key {
                    let comment = &rest[strip_comment(rest).len()..];
                    out.push(if comment.is_empty() {
                        format!("{key} = {value}")
                    } else {
                        format!("{key} = {value}  {comment}")
                    });
                    done = true;
                    continue;
                }
            }
        }
        out.push(raw.to_string());
    }

    if !done {
        let line = format!("{key} = {value}");
        match settings_hdr {
            Some(i) => out.insert(i + 1, line),
            None => {
                if out.last().is_some_and(|l| !l.trim().is_empty()) {
                    out.push(String::new());
                }
                out.push("[settings]".to_string());
                out.push(line);
            }
        }
    }

    let mut joined = out.join("\n");
    if text.ends_with('\n') || text.is_empty() {
        joined.push('\n');
    }
    joined
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            show_hidden: false,
            sort: SortKey::Natural,
            sort_reverse: false,
            sort_directories_first: true,
            sort_case_insensitive: true,
            column_ratios: vec![1, 3, 4],
            preview_files: true,
            preview_directories: true,
            preview_images: true,
            draw_borders: true,
            show_date: true,
            time_type: TimeType::Modified,
            time_format: TimeFormat::Date,
            size_format: SizeFormat::Human,
            theme: Theme::default(),
            confirm_on_delete: true,
            wrap_scroll: false,
            hidden_filter_dotfiles: true,
            openers: Vec::new(),
            preview_cmds: Vec::new(),
            preview_protocol: PreviewProtocol::Auto,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::style::Color;

    #[test]
    fn theme_name_and_per_role_override_parse() {
        let mut s = Settings::default();
        s.apply_toml("[settings]\ntheme = \"nord\"  # base palette\n\n[theme]\ndir = \"#ff0000\" # red\n");
        assert!(matches!(s.theme.bg, Color::Rgb { r: 46, g: 52, b: 64 }), "nord bg");
        assert!(matches!(s.theme.dir, Color::Rgb { r: 255, g: 0, b: 0 }), "dir override");
    }

    #[test]
    fn theme_override_is_order_independent() {
        let mut s = Settings::default();
        // [theme] appears before the base theme name; the base must not clobber it.
        s.apply_toml("[theme]\ndir = \"#00ff00\"\n\n[settings]\ntheme = \"dracula\"\n");
        assert!(matches!(s.theme.bg, Color::Rgb { r: 40, g: 42, b: 54 }), "dracula bg");
        assert!(matches!(s.theme.dir, Color::Rgb { r: 0, g: 255, b: 0 }), "dir override kept");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)] // deliberately mutate a few fields of a default
    fn default_config_template_parses_to_defaults() {
        // Applying the generated template to a mutated Settings restores defaults,
        // which also proves every key/value in the template is valid and current.
        let mut s = Settings::default();
        s.show_date = false;
        s.sort_directories_first = false;
        s.theme = Theme::by_name("nord").unwrap();
        s.apply_toml(DEFAULT_CONFIG);
        assert!(s.show_date);
        assert!(s.sort_directories_first);
        assert!(matches!(s.sort, SortKey::Natural));
        assert!(matches!(s.size_format, SizeFormat::Human));
        // theme = "default" keeps the terminal background.
        assert!(matches!(s.theme.bg, Color::Reset));
        // The template ships an active [preview] section (folio) for pdf/png/jpg.
        assert!(s.preview_cmds.iter().any(|(e, c)| e == "pdf" && c.contains("folio print")));
        assert!(s.preview_cmds.iter().any(|(e, _)| e == "png"));
        assert!(s.preview_cmds.iter().any(|(e, _)| e == "jpeg"));
        // Image preview is on by default.
        assert!(s.preview_images);
    }

    #[test]
    fn upsert_replaces_active_line_and_keeps_comment() {
        let text = "[settings]\npreview_images = true   # graphics preview\nshow_hidden = false\n";
        let out = upsert_setting(text, "preview_images", "false");
        assert!(out.contains("preview_images = false"));
        assert!(out.contains("# graphics preview"), "trailing comment preserved: {out:?}");
        assert!(!out.contains("preview_images = true"));
        // Untouched lines survive, trailing newline kept.
        assert!(out.contains("show_hidden = false"));
        assert!(out.ends_with('\n'));
        // The rewritten value round-trips through the parser.
        let mut s = Settings::default();
        s.apply_toml(&out);
        assert!(!s.preview_images);
    }

    #[test]
    fn upsert_uncomments_a_commented_key() {
        let text = "[settings]\n# preview_images = true\n";
        let out = upsert_setting(text, "preview_images", "false");
        assert!(out.contains("preview_images = false"));
        assert!(!out.contains("# preview_images"), "no longer commented: {out:?}");
    }

    #[test]
    fn upsert_inserts_missing_key_under_settings() {
        let text = "[settings]\nshow_hidden = true\n\n[preview]\npng = \"folio\"\n";
        let out = upsert_setting(text, "preview_images", "false");
        // Inserted under [settings], not into [preview].
        let settings_pos = out.find("[settings]").unwrap();
        let preview_pos = out.find("[preview]").unwrap();
        let key_pos = out.find("preview_images = false").expect("inserted");
        assert!(settings_pos < key_pos && key_pos < preview_pos, "placed in [settings]: {out:?}");
    }

    #[test]
    fn upsert_creates_settings_section_when_absent() {
        let out = upsert_setting("[open]\ncsv = \"x\"\n", "preview_images", "false");
        assert!(out.contains("[settings]"));
        assert!(out.contains("preview_images = false"));
        let mut s = Settings::default();
        s.apply_toml(&out);
        assert!(!s.preview_images);
    }

    #[test]
    fn open_section_maps_extensions_normalized() {
        fn find<'a>(s: &'a Settings, ext: &str) -> Option<&'a str> {
            s.openers.iter().find(|(e, _)| e == ext).map(|(_, c)| c.as_str())
        }
        let mut s = Settings::default();
        s.apply_toml("[open]\ncsv = \"rustranger\"\n.TSV = \"rustranger\"\npdf = \"zathura &\"\n");
        assert_eq!(find(&s, "csv"), Some("rustranger"));
        assert_eq!(find(&s, "tsv"), Some("rustranger")); // ".TSV" normalized to "tsv"
        assert_eq!(find(&s, "pdf"), Some("zathura &")); // value (incl. trailing &) preserved
        // A later entry for the same extension replaces an earlier one.
        s.apply_toml("[open]\ncsv = \"libreoffice --calc &\"\n");
        assert_eq!(find(&s, "csv"), Some("libreoffice --calc &"));
        assert_eq!(s.openers.iter().filter(|(e, _)| e == "csv").count(), 1);
    }

    #[test]
    fn preview_section_and_protocol_parse() {
        let mut s = Settings::default();
        s.apply_toml(
            "[settings]\npreview_protocol = \"kitty\"\n\n[preview]\nPNG = \"r %f\"\npdf = \"r %f\"\n",
        );
        // Parsed value overrides the auto default.
        assert_eq!(s.preview_protocol, PreviewProtocol::Kitty);
        assert_eq!(Settings::default().preview_protocol, PreviewProtocol::Auto);
        // Extension normalized to lowercase, no dot.
        assert_eq!(
            s.preview_cmds.iter().find(|(e, _)| e == "png").map(|(_, c)| c.as_str()),
            Some("r %f")
        );
        assert!(s.preview_cmds.iter().any(|(e, _)| e == "pdf"));
    }

    #[test]
    fn show_date_key_with_legacy_alias() {
        let mut s = Settings::default();
        s.set_field("show_date", "false");
        assert!(!s.show_date);
        s.set_field("show_time", "true"); // legacy alias still accepted
        assert!(s.show_date);
    }
}
