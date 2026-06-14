pub mod bookmarks;
pub mod history;
pub mod tags;

use std::path::PathBuf;

/// Directory for persistent data (bookmarks), following XDG.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(d).join("rustranger")
    } else if let Some(h) = std::env::var_os("HOME") {
        PathBuf::from(h).join(".local/share/rustranger")
    } else {
        PathBuf::from(".rustranger")
    }
}
