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
        // Pass 2: all settings (the theme name is already applied) + [theme] roles.
        for (section, key, value) in toml_entries(text) {
            match section {
                "settings" if key != "theme" => self.set_field(key, value),
                "theme" => {
                    if let Some(c) = crate::theme::parse_color(value) {
                        self.theme.set_field(key, c);
                    }
                }
                _ => {}
            }
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
/// of "settings", "theme", or "" (unknown); comments (`#`) and blanks are skipped.
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
draw_borders = true
confirm_on_delete = true
wrap_scroll = false
show_date = true               # date column next to the size (current column)
time_type = \"modified\"         # modified|created|changed|accessed
time_format = \"date\"           # date (YYYY/MM/DD) | datetime (YYYY/MM/DD/HH/MM)
size_format = \"human\"          # human (1.5k) | binary (1.5K, 1024-based) | bytes (1536)
theme = \"default\"              # default|gruvbox-dark|gruvbox-light|solarized-dark|
                               # solarized-light|nord|dracula|one-light|ayu-light

# Override individual theme roles on top of the chosen theme. Values may be
# #rrggbb, a basic color name, an ANSI index 0-255, or \"reset\".
# Roles: bg fg border title accent dir link exec special device broken
#        warning error info progress
# [theme]
# dir = \"#ff8800\"
# accent = \"cyan\"
# bg = \"#11131a\"
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
            draw_borders: true,
            show_date: true,
            time_type: TimeType::Modified,
            time_format: TimeFormat::Date,
            size_format: SizeFormat::Human,
            theme: Theme::default(),
            confirm_on_delete: true,
            wrap_scroll: false,
            hidden_filter_dotfiles: true,
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
