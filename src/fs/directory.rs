// A loaded directory: the files_all -> filter -> files pipeline with a sticky
// pointer, ported from ranger/container/directory.py.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::Settings;
use crate::fs::fsobject::Entry;
use crate::fs::sort::{sort_entries, SortOptions};

pub struct Dir {
    pub path: PathBuf,
    /// All entries as loaded from disk (already sorted).
    pub files_all: Vec<Entry>,
    /// Indices into `files_all` that are currently visible (after filtering).
    pub files: Vec<usize>,
    /// Cursor position: index into `files`.
    pub pointer: usize,
    pub loaded: bool,
    /// Optional substring/regex-free name filter (Phase 2 `:filter`).
    pub temporary_filter: Option<String>,
    /// mtime of the directory at load time, for outdated detection.
    load_mtime: Option<SystemTime>,
    pub error: Option<String>,
}

impl Dir {
    pub fn new(path: PathBuf) -> Dir {
        Dir {
            path,
            files_all: Vec::new(),
            files: Vec::new(),
            pointer: 0,
            loaded: false,
            temporary_filter: None,
            load_mtime: None,
            error: None,
        }
    }

    /// (Re)load the directory contents from disk.
    pub fn load(&mut self, settings: &Settings) {
        self.files_all.clear();
        self.error = None;

        match fs::read_dir(&self.path) {
            Ok(rd) => {
                for entry in rd.flatten() {
                    self.files_all.push(Entry::load(entry.path()));
                }
            }
            Err(e) => {
                self.error = Some(e.to_string());
            }
        }

        self.load_mtime = dir_mtime(&self.path);
        self.resort(settings);
        self.loaded = true;
    }

    /// Reload only if the directory changed on disk since last load.
    pub fn reload_if_outdated(&mut self, settings: &Settings) {
        let current = dir_mtime(&self.path);
        if current != self.load_mtime {
            let pointed = self.current().map(|e| e.name.clone());
            self.load(settings);
            if let Some(name) = pointed {
                self.select_name(&name);
            }
        }
    }

    /// Sort files_all then rebuild the visible list, preserving the pointed name.
    pub fn resort(&mut self, settings: &Settings) {
        let pointed = self.current().map(|e| e.name.clone());
        let opts = SortOptions {
            key: settings.sort,
            reverse: settings.sort_reverse,
            directories_first: settings.sort_directories_first,
            case_insensitive: settings.sort_case_insensitive,
        };
        sort_entries(&mut self.files_all, &opts);
        self.refilter(settings);
        if let Some(name) = pointed {
            self.select_name(&name);
        }
    }

    /// Rebuild `files` (visible indices) from `files_all` applying current filters.
    pub fn refilter(&mut self, settings: &Settings) {
        let pointed = self.current().map(|e| e.name.clone());
        self.files = self
            .files_all
            .iter()
            .enumerate()
            .filter(|(_, e)| self.is_visible(e, settings))
            .map(|(i, _)| i)
            .collect();
        self.clamp_pointer();
        if let Some(name) = pointed {
            self.select_name(&name);
        }
    }

    fn is_visible(&self, e: &Entry, settings: &Settings) -> bool {
        if !settings.show_hidden && settings.hidden_filter_dotfiles && e.name.starts_with('.') {
            return false;
        }
        if let Some(filter) = &self.temporary_filter {
            if !e
                .name
                .to_lowercase()
                .contains(&filter.to_lowercase())
            {
                return false;
            }
        }
        true
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The currently pointed entry, if any.
    pub fn current(&self) -> Option<&Entry> {
        self.files.get(self.pointer).map(|&i| &self.files_all[i])
    }

    pub fn current_mut(&mut self) -> Option<&mut Entry> {
        let idx = *self.files.get(self.pointer)?;
        Some(&mut self.files_all[idx])
    }

    /// Visible entries in display order.
    pub fn visible(&self) -> impl Iterator<Item = &Entry> {
        self.files.iter().map(move |&i| &self.files_all[i])
    }

    pub fn entry_at(&self, visible_index: usize) -> Option<&Entry> {
        self.files.get(visible_index).map(|&i| &self.files_all[i])
    }

    pub fn move_pointer(&mut self, delta: isize, wrap: bool) {
        if self.files.is_empty() {
            self.pointer = 0;
            return;
        }
        let len = self.files.len() as isize;
        let mut new = self.pointer as isize + delta;
        if wrap {
            new = ((new % len) + len) % len;
        } else {
            new = new.clamp(0, len - 1);
        }
        self.pointer = new as usize;
    }

    pub fn move_to(&mut self, index: usize) {
        if self.files.is_empty() {
            self.pointer = 0;
        } else {
            self.pointer = index.min(self.files.len() - 1);
        }
    }

    pub fn move_to_end(&mut self) {
        if !self.files.is_empty() {
            self.pointer = self.files.len() - 1;
        }
    }

    /// Place the cursor on the visible entry with the given name, if present.
    pub fn select_name(&mut self, name: &str) {
        let found = self.visible().position(|e| e.name == name);
        if let Some(pos) = found {
            self.pointer = pos;
        } else {
            self.clamp_pointer();
        }
    }

    // ---- marking ----------------------------------------------------------

    pub fn toggle_mark_at_pointer(&mut self) {
        if let Some(e) = self.current_mut() {
            e.marked = !e.marked;
        }
    }

    pub fn set_mark_at_pointer(&mut self, val: bool) {
        if let Some(e) = self.current_mut() {
            e.marked = val;
        }
    }

    pub fn toggle_all_marks(&mut self) {
        let indices: Vec<usize> = self.files.clone();
        for i in indices {
            self.files_all[i].marked = !self.files_all[i].marked;
        }
    }

    pub fn clear_marks(&mut self) {
        for e in &mut self.files_all {
            e.marked = false;
        }
    }

    #[allow(dead_code)]
    pub fn has_marks(&self) -> bool {
        self.visible().any(|e| e.marked)
    }

    /// Paths selected for an operation: marked visible entries, or the pointed one.
    pub fn selection(&self) -> Vec<PathBuf> {
        let marked: Vec<PathBuf> = self
            .visible()
            .filter(|e| e.marked)
            .map(|e| e.path.clone())
            .collect();
        if !marked.is_empty() {
            marked
        } else {
            self.current().map(|e| e.path.clone()).into_iter().collect()
        }
    }

    fn clamp_pointer(&mut self) {
        if self.files.is_empty() {
            self.pointer = 0;
        } else if self.pointer >= self.files.len() {
            self.pointer = self.files.len() - 1;
        }
    }
}

fn dir_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}
