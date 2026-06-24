pub mod opener;

use std::path::PathBuf;

/// A request to run an external program, handed back to the main loop so it can
/// suspend the TUI (for blocking programs like editors) or fork (for GUI apps).
pub struct RunRequest {
    pub argv: Vec<String>,
    /// Blocking programs (editor/pager/shell) need the TUI suspended.
    pub block: bool,
    /// True when the blocking program takes over the whole screen via its own
    /// alternate screen (editors, pagers, TUIs). For these the main loop keeps
    /// rustranger's alternate screen active across the handoff instead of dropping
    /// back to the primary buffer — so the terminal's default background never
    /// flashes through (dark→white→dark). Inline programs (`:shell`) leave it false
    /// so their output lands on the normal screen. Ignored when `block` is false.
    pub fullscreen: bool,
    /// Working directory to run in.
    pub cwd: PathBuf,
}
