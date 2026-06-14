// In-session file tags. Tags mark entries with a symbol shown in the file list.
// They are intentionally NOT persisted to disk and reset on each launch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DEFAULT_TAG: char = '*';

pub struct Tags {
    map: BTreeMap<PathBuf, char>,
}

impl Tags {
    pub fn new() -> Tags {
        Tags {
            map: BTreeMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn contains(&self, path: &Path) -> bool {
        self.map.contains_key(path)
    }

    pub fn marker(&self, path: &Path) -> Option<char> {
        self.map.get(path).copied()
    }

    /// Toggle the default tag on each path. If all are tagged, untag; else tag all.
    pub fn toggle(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let all_tagged = paths.iter().all(|p| self.map.contains_key(p));
        for p in paths {
            if all_tagged {
                self.map.remove(p);
            } else {
                self.map.insert(p.clone(), DEFAULT_TAG);
            }
        }
    }

    /// Keep a tag attached to a file that was renamed/moved within the session.
    pub fn update_path(&mut self, old: &Path, new: &Path) {
        if let Some(tag) = self.map.remove(old) {
            self.map.insert(new.to_path_buf(), tag);
        }
    }
}

impl Default for Tags {
    fn default() -> Self {
        Tags::new()
    }
}
