// Persistent file tags. Ported from ranger/container/tags.py.
// File format: one path per line, optionally "t:/path" where t is a tag symbol.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_TAG: char = '*';

pub struct Tags {
    map: BTreeMap<PathBuf, char>,
    file: PathBuf,
}

impl Tags {
    pub fn load(file: PathBuf) -> Tags {
        let mut map = BTreeMap::new();
        if let Ok(text) = fs::read_to_string(&file) {
            for line in text.lines() {
                let line = line.trim_end();
                if line.is_empty() {
                    continue;
                }
                // "t:/path" (tag symbol + colon) or a bare path with the default tag.
                if line.len() >= 2 && line.as_bytes()[1] == b':' {
                    let sym = line.chars().next().unwrap();
                    map.insert(PathBuf::from(&line[2..]), sym);
                } else {
                    map.insert(PathBuf::from(line), DEFAULT_TAG);
                }
            }
        }
        Tags { map, file }
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
        self.save();
    }

    #[allow(dead_code)]
    pub fn remove(&mut self, path: &Path) {
        if self.map.remove(path).is_some() {
            self.save();
        }
    }

    pub fn update_path(&mut self, old: &Path, new: &Path) {
        if let Some(tag) = self.map.remove(old) {
            self.map.insert(new.to_path_buf(), tag);
            self.save();
        }
    }

    fn save(&self) {
        if let Some(parent) = self.file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut out = String::new();
        for (p, sym) in &self.map {
            if *sym == DEFAULT_TAG {
                out.push_str(&format!("{}\n", p.display()));
            } else {
                out.push_str(&format!("{}:{}\n", sym, p.display()));
            }
        }
        let _ = fs::write(&self.file, out);
    }
}
