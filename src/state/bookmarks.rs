// Persistent single-key bookmarks. Ported from ranger/container/bookmarks.py.
// File format: one "k:/path/to/dir" per line.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Bookmarks {
    pub map: BTreeMap<char, PathBuf>,
    file: PathBuf,
}

impl Bookmarks {
    pub fn load(file: PathBuf) -> Bookmarks {
        let mut map = BTreeMap::new();
        if let Ok(text) = fs::read_to_string(&file) {
            for line in text.lines() {
                let line = line.trim_end();
                if let Some((key, path)) = line.split_once(':') {
                    let mut chars = key.chars();
                    if let (Some(k), None) = (chars.next(), chars.next()) {
                        if is_valid_key(k) {
                            map.insert(k, PathBuf::from(path));
                        }
                    }
                }
            }
        }
        Bookmarks { map, file }
    }

    pub fn set(&mut self, key: char, path: PathBuf) {
        if is_valid_key(key) {
            self.map.insert(key, path);
            self.save();
        }
    }

    pub fn get(&self, key: char) -> Option<&PathBuf> {
        self.map.get(&key)
    }

    pub fn delete(&mut self, key: char) {
        if self.map.remove(&key).is_some() {
            self.save();
        }
    }

    /// Remove every bookmark.
    pub fn clear(&mut self) {
        if !self.map.is_empty() {
            self.map.clear();
            self.save();
        }
    }

    fn save(&self) {
        if let Some(parent) = self.file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut out = String::new();
        for (k, p) in &self.map {
            out.push_str(&format!("{}:{}\n", k, p.display()));
        }
        let _ = fs::write(&self.file, out);
    }

    /// Update bookmarks after a directory is renamed/moved.
    pub fn update_path(&mut self, old: &Path, new: &Path) {
        let mut changed = false;
        for p in self.map.values_mut() {
            if p == old {
                *p = new.to_path_buf();
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }
}

fn is_valid_key(k: char) -> bool {
    k.is_ascii_alphanumeric() || k == '`' || k == '\''
}
