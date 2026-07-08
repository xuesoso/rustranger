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
        // Write to a sibling temp file, then rename over the target: the rename
        // is atomic on POSIX, so a crash mid-save can truncate at worst the
        // temp file — never the existing bookmarks.
        let tmp = self.file.with_extension("tmp");
        if fs::write(&tmp, out).is_ok() {
            let _ = fs::rename(&tmp, &self.file);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Saving must round-trip through the file and leave no temp file behind
    /// (the atomic write-then-rename path).
    #[test]
    fn save_roundtrips_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("rr_bm_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bookmarks");

        let mut bm = Bookmarks::load(file.clone());
        bm.set('a', PathBuf::from("/tmp"));
        bm.set('b', PathBuf::from("/home"));
        assert!(!dir.join("bookmarks.tmp").exists(), "temp file must not linger");

        let re = Bookmarks::load(file);
        assert_eq!(re.get('a'), Some(&PathBuf::from("/tmp")));
        assert_eq!(re.get('b'), Some(&PathBuf::from("/home")));

        let _ = fs::remove_dir_all(&dir);
    }
}
