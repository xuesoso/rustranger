// A browsing tab: an independent current directory + navigation history.
// Ported from ranger/core/tab.py (directory caching itself stays shared in App).

use std::path::PathBuf;

use crate::state::history::History;

pub struct Tab {
    pub cwd: PathBuf,
    pub history: History,
}

impl Tab {
    pub fn new(cwd: PathBuf) -> Tab {
        Tab {
            history: History::new(cwd.clone()),
            cwd,
        }
    }
}
