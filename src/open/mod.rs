pub mod opener;

use std::path::PathBuf;

/// A request to run an external program, handed back to the main loop so it can
/// suspend the TUI (for blocking programs like editors) or fork (for GUI apps).
pub struct RunRequest {
    pub argv: Vec<String>,
    /// Blocking programs (editor/pager/shell) need the TUI suspended.
    pub block: bool,
    /// Working directory to run in.
    pub cwd: PathBuf,
}
