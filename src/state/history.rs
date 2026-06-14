// Per-tab directory navigation history. Ported from ranger/container/history.py.

use std::path::PathBuf;

const MAX_LEN: usize = 100;

pub struct History {
    entries: Vec<PathBuf>,
    index: usize,
}

impl History {
    pub fn new(start: PathBuf) -> History {
        History {
            entries: vec![start],
            index: 0,
        }
    }

    /// Record a navigation to `path`, discarding any forward entries.
    pub fn add(&mut self, path: PathBuf) {
        if self.entries.get(self.index) == Some(&path) {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(path);
        if self.entries.len() > MAX_LEN {
            let overflow = self.entries.len() - MAX_LEN;
            self.entries.drain(0..overflow);
        }
        self.index = self.entries.len() - 1;
    }

    pub fn back(&mut self) -> Option<PathBuf> {
        if self.index == 0 {
            return None;
        }
        self.index -= 1;
        self.entries.get(self.index).cloned()
    }

    pub fn forward(&mut self) -> Option<PathBuf> {
        if self.index + 1 >= self.entries.len() {
            return None;
        }
        self.index += 1;
        self.entries.get(self.index).cloned()
    }
}
