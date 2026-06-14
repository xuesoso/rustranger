// Settings that affect browsing. Hardcoded defaults mirror ranger's rc.conf.
// Phase 7 adds optional TOML overrides on top of these defaults.

use crate::fs::sort::SortKey;

#[derive(Clone)]
pub struct Settings {
    pub show_hidden: bool,
    pub sort: SortKey,
    pub sort_reverse: bool,
    pub sort_directories_first: bool,
    pub sort_case_insensitive: bool,
    pub scroll_offset: usize,
    pub column_ratios: Vec<u32>,
    pub preview_files: bool,
    pub preview_directories: bool,
    pub draw_borders: bool,
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

    /// Apply a flat, minimal subset of TOML: `key = value` lines under a
    /// `[settings]` section. Avoids pulling in a TOML crate for a flat schema.
    fn apply_toml(&mut self, text: &str) {
        let mut in_settings = false;
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                in_settings = line == "[settings]";
                continue;
            }
            if !in_settings {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            self.set_field(key, value);
        }
    }

    fn set_field(&mut self, key: &str, value: &str) {
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
            "scroll_offset" => {
                if let Ok(n) = value.parse() {
                    self.scroll_offset = n;
                }
            }
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
            "confirm_on_delete" => self.confirm_on_delete = as_bool(value),
            "wrap_scroll" => self.wrap_scroll = as_bool(value),
            _ => {}
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let base = if let Some(d) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(d)
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".config")
    };
    Some(base.join("rustranger").join("config.toml"))
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            show_hidden: false,
            sort: SortKey::Natural,
            sort_reverse: false,
            sort_directories_first: true,
            sort_case_insensitive: true,
            scroll_offset: 8,
            column_ratios: vec![1, 3, 4],
            preview_files: true,
            preview_directories: true,
            draw_borders: true,
            confirm_on_delete: true,
            wrap_scroll: false,
            hidden_filter_dotfiles: true,
        }
    }
}
