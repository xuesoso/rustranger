// Central application state. Phase 1: a directory cache + cursor navigation.
// Later phases add tabs, marking, file ops, console, etc.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::Settings;
use crate::console::ConsoleState;
use crate::fs::sort::SortKey;
use crate::fs::{Dir, FType};
use crate::open::opener;
use crate::open::RunRequest;
use crate::ops::copy::{self, CopyJob};
use crate::ops::fileops;
use crate::preview::{self, Preview};
use crate::state::bookmarks::Bookmarks;
use crate::state::tags::Tags;
use crate::state::data_dir;
use crate::tab::Tab;

/// Minimum interval between on-disk freshness checks of the visible directories.
/// `prepare_view` runs once per frame; without a throttle, an external process
/// writing into a large directory would force a full reload on every frame.
const AUTO_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// Cache caps: past these sizes the least-recently-used cached directories and
/// previews are evicted, so a long browsing session cannot grow memory without
/// bound. Everything on screen (each tab's cwd and parent, the previewed
/// entry) is protected from eviction.
const DIR_CACHE_MAX: usize = 32;
const PREVIEW_CACHE_MAX: usize = 128;

/// A cached file preview: the (mtime_ns, size) stamp the file had when read —
/// nanosecond resolution so a modification within the same second still
/// invalidates, with size as a backstop for coarse-timestamp filesystems —
/// plus an access stamp for LRU eviction.
pub struct PreviewSlot {
    stamp: (i64, u64),
    at: u64,
    pub preview: Preview,
}

/// A pending action awaiting y/n confirmation in the status bar.
pub enum Confirm {
    Delete(Vec<PathBuf>),
}

/// A transient hint overlay shown while a multi-key prefix is pending, listing
/// the keys that complete the chord (ranger pops up the same when you press `o`,
/// `g`, etc.). Owns its rows so dynamic menus (bookmarks) can be built on the fly.
pub struct KeyMenu {
    pub title: String,
    /// (key, description) rows, in display order.
    pub items: Vec<(String, String)>,
}

impl KeyMenu {
    fn from_static(title: &str, items: &[(&str, &str)]) -> KeyMenu {
        KeyMenu {
            title: title.to_string(),
            items: items
                .iter()
                .map(|(k, d)| (k.to_string(), d.to_string()))
                .collect(),
        }
    }

    /// The sort menu shown after pressing `o`. Following ranger, the lowercase
    /// key sorts ascending and the SHIFTed (uppercase) key sorts descending.
    pub fn sort() -> KeyMenu {
        KeyMenu::from_static(
            "sort by  (UPPERCASE = reversed)",
            &[
                ("s/S", "size"),
                ("n/N", "natural"),
                ("b/B", "basename"),
                ("m/M", "mtime"),
                ("c/C", "ctime"),
                ("a/A", "atime"),
                ("t/T", "type"),
                ("e/E", "extension"),
                ("z", "random"),
                ("r", "toggle reverse"),
                ("f", "toggle dirs-first"),
            ],
        )
    }

    /// The `g` (go) navigation menu.
    pub fn go() -> KeyMenu {
        KeyMenu::from_static(
            "go",
            &[
                ("g", "top"),
                ("h", "home (~)"),
                ("/", "root (/)"),
                ("n", "new tab"),
                ("t", "next tab"),
                ("T", "previous tab"),
            ],
        )
    }

    /// The `y` (yank/copy) menu.
    pub fn yank() -> KeyMenu {
        KeyMenu::from_static("yank", &[("y", "copy selection")])
    }

    /// The `d` (cut) menu.
    pub fn cut() -> KeyMenu {
        KeyMenu::from_static("cut", &[("d", "cut selection")])
    }

    /// The `p` (paste) menu.
    pub fn paste() -> KeyMenu {
        KeyMenu::from_static(
            "paste",
            &[
                ("p", "paste"),
                ("l", "paste symlink (relative)"),
                ("L", "paste symlink (absolute)"),
                ("h", "paste hardlink"),
            ],
        )
    }

    /// The `u` (un-) menu.
    pub fn un() -> KeyMenu {
        KeyMenu::from_static("un-", &[("v", "clear marks"), ("m", "delete bookmark")])
    }

    /// The `c` (change) menu.
    pub fn change() -> KeyMenu {
        KeyMenu::from_static("change", &[("w", "rename")])
    }
}

pub struct App {
    pub settings: Settings,
    pub dirs: HashMap<PathBuf, Dir>,
    pub tabs: Vec<Tab>,
    pub current_tab: usize,
    pub quit: bool,
    pub message: Option<String>,
    /// Cached file previews keyed by path (see [`PreviewSlot`]).
    pub previews: HashMap<PathBuf, PreviewSlot>,
    /// Vertical scroll offset of the preview pane.
    pub preview_scroll: usize,
    /// Path the preview pane is currently showing (to reset scroll on change).
    preview_path: Option<PathBuf>,
    /// Files selected for copy/cut, and whether the pending paste is a move.
    pub copy_buffer: Vec<PathBuf>,
    pub do_cut: bool,
    /// Running background copy/move jobs.
    pub jobs: Vec<CopyJob>,
    /// Visual-selection mode: moving the cursor marks entries it passes.
    pub visual: bool,
    /// A pending confirmation prompt.
    pub confirm: Option<Confirm>,
    /// A transient key-chain hint menu (e.g. sort options after pressing `o`).
    pub menu: Option<KeyMenu>,
    /// The scrollable help overlay, holding its scroll offset when open.
    pub help: Option<usize>,
    pub bookmarks: Bookmarks,
    pub tags: Tags,
    /// Active `:`/`/` console line editor, if any.
    pub console: Option<ConsoleState>,
    /// Last search term for n/N.
    pub search_term: String,
    /// A pending external-program run, picked up by the main loop.
    pub pending_run: Option<RunRequest>,
    /// File-picker mode: when set, choosing a file writes its path here and quits.
    pub choosefile: Option<PathBuf>,
    /// When the visible directories were last checked against the disk
    /// (throttles the auto-refresh in `prepare_view`). None = never checked.
    last_fs_check: Option<Instant>,
    /// Monotonic counter stamped onto cache entries on access (LRU eviction).
    access_clock: u64,
}

impl App {
    pub fn new(start: PathBuf, settings: Settings) -> App {
        let dir = data_dir();
        let mut app = App {
            settings,
            dirs: HashMap::new(),
            tabs: vec![Tab::new(start.clone())],
            current_tab: 0,
            quit: false,
            message: None,
            previews: HashMap::new(),
            preview_scroll: 0,
            preview_path: None,
            copy_buffer: Vec::new(),
            do_cut: false,
            jobs: Vec::new(),
            visual: false,
            confirm: None,
            menu: None,
            help: None,
            bookmarks: Bookmarks::load(dir.join("bookmarks")),
            tags: Tags::new(),
            console: None,
            search_term: String::new(),
            pending_run: None,
            choosefile: None,
            last_fs_check: None,
            access_clock: 0,
        };
        app.ensure_dir(&start);
        app
    }

    /// The active tab's current directory path.
    pub fn cwd(&self) -> PathBuf {
        self.tabs[self.current_tab].cwd.clone()
    }

    /// Load a directory into the cache if not already present, and stamp it as
    /// recently used (the stamp drives the LRU eviction in `evict_caches`).
    pub fn ensure_dir(&mut self, path: &Path) {
        if !self.dirs.contains_key(path) {
            let mut dir = Dir::new(path.to_path_buf());
            dir.load(&self.settings);
            self.dirs.insert(path.to_path_buf(), dir);
        }
        self.access_clock += 1;
        let clock = self.access_clock;
        if let Some(d) = self.dirs.get_mut(path) {
            d.last_access = clock;
        }
    }

    /// Bound both caches: evict least-recently-used entries past the caps,
    /// never touching what is on screen (each tab's cwd and its parent, plus
    /// the pointed entry, whose directory fills the preview column).
    fn evict_caches(&mut self) {
        if self.dirs.len() > DIR_CACHE_MAX {
            let mut keep: HashSet<PathBuf> = HashSet::new();
            for tab in &self.tabs {
                keep.insert(tab.cwd.clone());
                if let Some(p) = tab.cwd.parent() {
                    keep.insert(p.to_path_buf());
                }
            }
            if let Some(sel) = self.selected_path() {
                keep.insert(sel);
            }
            while self.dirs.len() > DIR_CACHE_MAX {
                let victim = self
                    .dirs
                    .iter()
                    .filter(|(p, _)| !keep.contains(p.as_path()))
                    .min_by_key(|(_, d)| d.last_access)
                    .map(|(p, _)| p.clone());
                match victim {
                    Some(p) => {
                        self.dirs.remove(&p);
                    }
                    None => break,
                }
            }
        }
        while self.previews.len() > PREVIEW_CACHE_MAX {
            let victim = self
                .previews
                .iter()
                .min_by_key(|(_, s)| s.at)
                .map(|(p, _)| p.clone());
            match victim {
                Some(p) => {
                    self.previews.remove(&p);
                }
                None => break,
            }
        }
    }

    /// Drop `path` and everything under it from the directory and preview caches.
    /// Called when a path is removed so a later recreation of the same path is not
    /// served from a stale cached listing.
    fn invalidate_cache(&mut self, path: &Path) {
        self.dirs.retain(|p, _| p != path && !p.starts_with(path));
        self.previews.retain(|p, _| p != path && !p.starts_with(path));
    }

    pub fn current_dir(&self) -> &Dir {
        self.dirs.get(&self.cwd()).expect("cwd must be loaded")
    }

    pub fn get_cached(&self, path: &Path) -> Option<&Dir> {
        self.dirs.get(path)
    }

    pub fn parent_path(&self) -> Option<PathBuf> {
        self.cwd().parent().map(|p| p.to_path_buf())
    }

    /// The entry currently under the cursor (cloned path), if any.
    pub fn selected_path(&self) -> Option<PathBuf> {
        self.current_dir().current().map(|e| e.path.clone())
    }

    /// Ensure the directories needed to render the miller view (parent + a
    /// directory preview) are loaded into the cache. Called once per frame.
    pub fn prepare_view(&mut self) {
        // Auto-refresh: pick up changes made to the visible directories by
        // other programs (files created/deleted/renamed bump the dir mtime).
        // Throttled so bursts of frames — fast scrolling, job-progress ticks —
        // don't re-stat and potentially reload on every frame.
        let due = match self.last_fs_check {
            Some(t) => t.elapsed() >= AUTO_REFRESH_INTERVAL,
            None => true,
        };
        if due {
            self.last_fs_check = Some(Instant::now());
            self.refresh_from_disk();
        }

        // Keep the cwd loaded (self-healing if anything dropped it) and stamp
        // it as recently used so eviction can never take the active column.
        let cwd = self.cwd();
        self.ensure_dir(&cwd);

        if let Some(parent) = self.parent_path() {
            self.ensure_dir(&parent);
            // Keep the parent column's cursor on the directory we're inside.
            if let Some(name) = self.cwd().file_name().map(|s| s.to_string_lossy().into_owned()) {
                if let Some(dir) = self.dirs.get_mut(&parent) {
                    dir.select_name(&name);
                }
            }
        }
        // Resolve the selected entry once for both directory and file previews.
        let selected = self
            .current_dir()
            .current()
            .map(|e| (e.path.clone(), e.is_dir(), e.accessible, e.ftype));

        // Reset preview scroll when the pointed entry changes.
        let sel_path = selected.as_ref().map(|s| s.0.clone());
        if sel_path != self.preview_path {
            self.preview_scroll = 0;
            self.preview_path = sel_path;
        }

        match selected {
            Some((path, true, true, _)) => {
                self.ensure_dir(&path);
                // The previewed directory may have changed on disk since it was
                // cached (e.g. deleted and recreated, or modified externally);
                // refresh it if its mtime moved so the preview isn't stale.
                let settings = self.settings.clone();
                if let Some(dir) = self.dirs.get_mut(&path) {
                    dir.reload_if_outdated(&settings);
                }
            }
            Some((path, false, _, ftype)) => {
                if matches!(ftype, FType::File) {
                    // A content edit doesn't bump the parent dir's mtime, so the
                    // cached entry (and the preview keyed off its mtime) would
                    // stay stale forever: re-stat the pointed file each frame.
                    let (size, mtime) = self.refresh_selected_file(&path);
                    if self.settings.preview_files {
                        self.ensure_preview(&path, size, mtime);
                    }
                }
            }
            _ => {}
        }

        self.evict_caches();
    }

    /// Reload the current directory and its parent when they changed on disk —
    /// files created, deleted, or renamed by something other than rustranger
    /// (another shell, a build, a finished download). Cheap when nothing
    /// changed: one metadata stat per directory. Together with the previewed
    /// directory's per-frame check in `prepare_view` and the re-check in `cd`,
    /// every visible column tracks the disk.
    pub fn refresh_from_disk(&mut self) {
        let settings = self.settings.clone();
        let cwd = self.cwd();
        if let Some(dir) = self.dirs.get_mut(&cwd) {
            dir.reload_if_outdated(&settings);
        }
        if let Some(parent) = self.parent_path() {
            if let Some(dir) = self.dirs.get_mut(&parent) {
                dir.reload_if_outdated(&settings);
            }
        }
    }

    /// Re-stat the pointed regular file, refreshing its cached entry in place
    /// (marks preserved). Returns the fresh (size, mtime_ns) for the preview cache.
    fn refresh_selected_file(&mut self, path: &Path) -> (u64, i64) {
        let fresh = crate::fs::Entry::load(path.to_path_buf());
        let stat = (fresh.size, fresh.mtime_ns);
        if let Some(e) = self.current_dir_mut().current_mut() {
            if e.path == *path {
                let marked = e.marked;
                *e = fresh;
                e.marked = marked;
            }
        }
        stat
    }

    /// Load (or refresh, if the file changed) the preview for a file path.
    fn ensure_preview(&mut self, path: &Path, size: u64, mtime_ns: i64) {
        self.access_clock += 1;
        let (at, stamp) = (self.access_clock, (mtime_ns, size));
        match self.previews.get_mut(path) {
            Some(slot) if slot.stamp == stamp => slot.at = at,
            _ => {
                let preview = preview::load(path, size);
                self.previews.insert(path.to_path_buf(), PreviewSlot { stamp, at, preview });
            }
        }
    }

    pub fn current_preview(&self) -> Option<&Preview> {
        let path = self.preview_path.as_ref()?;
        self.previews.get(path).map(|s| &s.preview)
    }

    pub fn scroll_preview(&mut self, delta: isize) {
        let new = self.preview_scroll as isize + delta;
        self.preview_scroll = new.max(0) as usize;
    }

    pub fn current_dir_mut(&mut self) -> &mut Dir {
        let cwd = self.cwd();
        self.dirs.get_mut(&cwd).expect("cwd must be loaded")
    }

    /// Re-sort/re-filter every cached directory after a settings change.
    pub fn refresh_all(&mut self) {
        let settings = self.settings.clone();
        for dir in self.dirs.values_mut() {
            dir.resort(&settings);
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let wrap = self.settings.wrap_scroll;
        let visual = self.visual;
        let dir = self.current_dir_mut();
        // In visual mode, mark every entry the cursor steps onto (and the start).
        if visual {
            dir.set_mark_at_pointer(true);
            let steps = delta.unsigned_abs();
            let unit = if delta >= 0 { 1 } else { -1 };
            for _ in 0..steps {
                dir.move_pointer(unit, wrap);
                dir.set_mark_at_pointer(true);
            }
        } else {
            dir.move_pointer(delta, wrap);
        }
    }

    pub fn move_to_top(&mut self) {
        self.current_dir_mut().move_to(0);
    }

    pub fn move_to_bottom(&mut self) {
        self.current_dir_mut().move_to_end();
    }

    /// Enter the pointed directory, or open the pointed file.
    pub fn enter(&mut self) {
        let entry = self
            .current_dir()
            .current()
            .map(|e| (e.path.clone(), e.is_dir(), e.accessible, e.ftype));
        match entry {
            Some((path, true, true, _)) => self.cd(path),
            Some((path, false, _, ftype)) => self.open_path(&path, ftype),
            _ => {}
        }
    }

    fn open_path(&mut self, path: &Path, ftype: FType) {
        // In file-picker mode, write the chosen path and exit instead of opening.
        if let Some(out) = self.choosefile.clone() {
            let _ = std::fs::write(&out, format!("{}\n", path.display()));
            self.quit = true;
            return;
        }
        // Only regular files are opened. A FIFO would block the opener's content
        // sniff (a plain `open()` on a pipe waits for a writer) — freezing the
        // whole TUI with no way to interrupt it from inside raw mode — and
        // sockets/devices are not meaningful to hand to an editor or xdg-open.
        if !matches!(ftype, FType::File) {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.message = Some(format!("cannot open {}: {}", name, ftype.name()));
            return;
        }
        self.pending_run = Some(opener::open_file(path, self.cwd(), &self.settings.openers));
    }

    /// Go to the parent directory, keeping the cursor on the directory we left.
    pub fn ascend(&mut self) {
        let cwd = self.cwd();
        if let Some(parent) = cwd.parent().map(|p| p.to_path_buf()) {
            let leaving = cwd.file_name().map(|s| s.to_string_lossy().into_owned());
            self.cd(parent);
            if let Some(name) = leaving {
                self.current_dir_mut().select_name(&name);
            }
        }
    }

    pub fn cd(&mut self, path: PathBuf) {
        self.ensure_dir(&path);
        let settings = self.settings.clone();
        if let Some(dir) = self.dirs.get_mut(&path) {
            dir.reload_if_outdated(&settings);
        }
        let tab = &mut self.tabs[self.current_tab];
        tab.cwd = path.clone();
        tab.history.add(path);
    }

    /// Change directory without recording history (used by history navigation).
    fn set_cwd_no_history(&mut self, path: PathBuf) {
        self.ensure_dir(&path);
        let settings = self.settings.clone();
        if let Some(dir) = self.dirs.get_mut(&path) {
            dir.reload_if_outdated(&settings);
        }
        self.tabs[self.current_tab].cwd = path;
    }

    pub fn toggle_hidden(&mut self) {
        self.settings.show_hidden = !self.settings.show_hidden;
        let settings = self.settings.clone();
        for dir in self.dirs.values_mut() {
            dir.refilter(&settings);
        }
    }

    pub fn set_sort(&mut self, key: SortKey) {
        self.settings.sort = key;
        self.message = Some(format!("sort: {}", key.name()));
        self.refresh_all();
    }

    /// Set the sort key *and* direction in one step (ranger's `os`/`oS` chords:
    /// lowercase = ascending, SHIFTed uppercase = descending).
    pub fn set_sort_order(&mut self, key: SortKey, reverse: bool) {
        self.settings.sort = key;
        self.settings.sort_reverse = reverse;
        self.message = Some(format!(
            "sort: {} ({})",
            key.name(),
            if reverse { "reversed" } else { "normal" }
        ));
        self.refresh_all();
    }

    pub fn toggle_sort_reverse(&mut self) {
        self.settings.sort_reverse = !self.settings.sort_reverse;
        self.message = Some(format!("reverse: {}", self.settings.sort_reverse));
        self.refresh_all();
    }

    pub fn toggle_dirs_first(&mut self) {
        self.settings.sort_directories_first = !self.settings.sort_directories_first;
        self.refresh_all();
    }

    // ---- marking / visual -------------------------------------------------

    pub fn toggle_mark(&mut self) {
        self.current_dir_mut().toggle_mark_at_pointer();
        self.move_cursor(1);
    }

    pub fn toggle_all_marks(&mut self) {
        self.current_dir_mut().toggle_all_marks();
    }

    pub fn clear_marks(&mut self) {
        self.current_dir_mut().clear_marks();
        self.visual = false;
    }

    pub fn toggle_visual(&mut self) {
        self.visual = !self.visual;
        if self.visual {
            // Mark the entry we start on.
            self.current_dir_mut().set_mark_at_pointer(true);
            self.message = Some("-- VISUAL --".to_string());
        }
    }

    // ---- copy / cut / paste ----------------------------------------------

    pub fn copy(&mut self) {
        self.copy_buffer = self.current_dir().selection();
        self.do_cut = false;
        self.message = Some(format!("copied {} item(s)", self.copy_buffer.len()));
        self.current_dir_mut().clear_marks();
        self.visual = false;
    }

    pub fn cut(&mut self) {
        self.copy_buffer = self.current_dir().selection();
        self.do_cut = true;
        self.message = Some(format!("cut {} item(s)", self.copy_buffer.len()));
        self.current_dir_mut().clear_marks();
        self.visual = false;
    }

    pub fn paste(&mut self) {
        if self.copy_buffer.is_empty() {
            self.message = Some("copy buffer is empty".to_string());
            return;
        }
        let sources = self.copy_buffer.clone();
        let dest = self.cwd();
        let cut = self.do_cut;
        self.jobs.push(copy::start(sources, dest, cut));
        if cut {
            // A move consumes the buffer; a copy can be pasted repeatedly.
            self.copy_buffer.clear();
            self.do_cut = false;
        }
    }

    pub fn paste_links(&mut self, relative: bool) {
        let dest = self.cwd();
        let mut errors = 0;
        for src in &self.copy_buffer.clone() {
            if fileops::symlink_into(src, &dest, relative).is_err() {
                errors += 1;
            }
        }
        self.message = Some(if errors == 0 {
            "linked".to_string()
        } else {
            format!("{} link(s) failed", errors)
        });
        self.reload_dir(&self.cwd());
    }

    pub fn paste_hardlinks(&mut self) {
        let dest = self.cwd();
        let mut errors = 0;
        for src in &self.copy_buffer.clone() {
            if fileops::hardlink_into(src, &dest).is_err() {
                errors += 1;
            }
        }
        self.message = Some(if errors == 0 {
            "hardlinked".to_string()
        } else {
            format!("{} hardlink(s) failed", errors)
        });
        self.reload_dir(&self.cwd());
    }

    // ---- delete -----------------------------------------------------------

    /// Stage a delete of the current selection, requesting confirmation.
    pub fn request_delete(&mut self) {
        let targets = self.current_dir().selection();
        if targets.is_empty() {
            return;
        }
        if self.settings.confirm_on_delete {
            self.message = Some(format!(
                "delete {} item(s)? (y/N)",
                targets.len()
            ));
            self.confirm = Some(Confirm::Delete(targets));
        } else {
            self.perform_delete(targets);
        }
    }

    pub fn answer_confirm(&mut self, yes: bool) {
        if let Some(confirm) = self.confirm.take() {
            if yes {
                match confirm {
                    Confirm::Delete(targets) => self.perform_delete(targets),
                }
            } else {
                self.message = Some("aborted".to_string());
            }
        }
    }

    fn perform_delete(&mut self, targets: Vec<PathBuf>) {
        // Decide which entry to focus afterwards (nearest survivor, preferring the
        // one above) while the to-be-deleted entries are still in the listing.
        let deleted: HashSet<PathBuf> = targets.iter().cloned().collect();
        let anchor = self.current_dir().survivor_name(&deleted);

        let mut errors = 0;
        for t in &targets {
            if fileops::delete_path(t).is_err() {
                errors += 1;
            } else {
                // Drop any cached listing/preview for the removed path so a later
                // `mkdir` of the same name doesn't show the old contents.
                self.invalidate_cache(t);
            }
        }
        self.message = Some(if errors == 0 {
            format!("deleted {} item(s)", targets.len())
        } else {
            format!("{} delete(s) failed", errors)
        });
        let cwd = self.cwd();
        self.reload_dir(&cwd);
        // Move the cursor to the chosen survivor (the dir may be empty, in which
        // case there is nothing to select and the cursor stays at 0).
        if let Some(name) = anchor {
            self.current_dir_mut().select_name(&name);
        }
    }

    // ---- background job ticking ------------------------------------------

    /// Poll running jobs; returns true if any are still active (needs redraw).
    pub fn tick_jobs(&mut self) -> bool {
        if self.jobs.is_empty() {
            return false;
        }
        let mut finished_dests: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        self.jobs.retain_mut(|job| {
            let running = job.poll();
            if !running {
                finished_dests.push(job.dest.clone());
                if let Some(err) = &job.progress().error {
                    errors.push(err.clone());
                }
            }
            running
        });
        if !finished_dests.is_empty() {
            // Reload destinations and the current directory (source of a move).
            let cwd = self.cwd();
            self.reload_dir(&cwd);
            for d in finished_dests {
                self.reload_dir(&d);
            }
            // A failed copy/move must say so — not "complete".
            self.message = Some(if errors.is_empty() {
                "operation complete".to_string()
            } else {
                format!("operation failed: {}", errors.join("; "))
            });
        }
        !self.jobs.is_empty()
    }

    pub fn jobs_active(&self) -> bool {
        !self.jobs.is_empty()
    }

    fn reload_dir(&mut self, path: &Path) {
        let settings = self.settings.clone();
        if let Some(dir) = self.dirs.get_mut(path) {
            dir.load(&settings);
        }
    }

    // ---- history ----------------------------------------------------------

    pub fn history_back(&mut self) {
        if let Some(path) = self.tabs[self.current_tab].history.back() {
            self.set_cwd_no_history(path);
        }
    }

    pub fn history_forward(&mut self) {
        if let Some(path) = self.tabs[self.current_tab].history.forward() {
            self.set_cwd_no_history(path);
        }
    }

    // ---- tabs -------------------------------------------------------------

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn tab_new(&mut self) {
        let cwd = self.cwd();
        self.tabs.insert(self.current_tab + 1, Tab::new(cwd));
        self.current_tab += 1;
    }

    /// Close the active tab. If it's the last one, quit.
    pub fn tab_close(&mut self) {
        if self.tabs.len() <= 1 {
            self.quit = true;
            return;
        }
        self.tabs.remove(self.current_tab);
        if self.current_tab >= self.tabs.len() {
            self.current_tab = self.tabs.len() - 1;
        }
    }

    pub fn tab_next(&mut self) {
        if !self.tabs.is_empty() {
            self.current_tab = (self.current_tab + 1) % self.tabs.len();
        }
    }

    pub fn tab_prev(&mut self) {
        if !self.tabs.is_empty() {
            self.current_tab = (self.current_tab + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    /// Switch to tab number `n` (1-based); create a new tab if it doesn't exist.
    pub fn tab_goto(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        if n <= self.tabs.len() {
            self.current_tab = n - 1;
        } else {
            let cwd = self.cwd();
            self.tabs.push(Tab::new(cwd));
            self.current_tab = self.tabs.len() - 1;
        }
    }

    // ---- bookmarks --------------------------------------------------------

    /// Build the hint menu listing current bookmarks (shown after `m`, `` ` ``, or
    /// `um`). `title` labels the action (set / go to / delete).
    pub fn bookmark_menu(&self, title: &str) -> KeyMenu {
        let mut items: Vec<(String, String)> = self
            .bookmarks
            .map
            .iter()
            .map(|(k, p)| (k.to_string(), p.display().to_string()))
            .collect();
        if items.is_empty() {
            items.push((String::new(), "(no bookmarks set)".to_string()));
        }
        KeyMenu {
            title: title.to_string(),
            items,
        }
    }

    pub fn set_bookmark(&mut self, key: char) {
        let cwd = self.cwd();
        self.bookmarks.set(key, cwd);
        self.message = Some(format!("bookmark '{}' set", key));
    }

    pub fn enter_bookmark(&mut self, key: char) {
        if let Some(path) = self.bookmarks.get(key).cloned() {
            if path.is_dir() {
                self.cd(path);
            } else {
                self.message = Some(format!("bookmark '{}' is gone", key));
            }
        } else {
            self.message = Some(format!("no bookmark '{}'", key));
        }
    }

    pub fn delete_bookmark(&mut self, key: char) {
        if self.bookmarks.get(key).is_some() {
            self.bookmarks.delete(key);
            self.message = Some(format!("bookmark '{}' deleted", key));
        } else {
            self.message = Some(format!("no bookmark '{}'", key));
        }
    }

    pub fn clear_bookmarks(&mut self) {
        self.bookmarks.clear();
        self.message = Some("bookmarks cleared".to_string());
    }

    // ---- tags -------------------------------------------------------------

    pub fn toggle_tag(&mut self) {
        let targets = self.current_dir().selection();
        self.tags.toggle(&targets);
        self.current_dir_mut().clear_marks();
        self.visual = false;
    }

    // ---- console ----------------------------------------------------------

    pub fn open_console(&mut self, prompt: char, initial: &str) {
        self.console = Some(ConsoleState::new(prompt, initial));
    }

    pub fn console_cancel(&mut self) {
        self.console = None;
    }

    /// Execute the console line and close it.
    pub fn console_submit(&mut self) {
        let Some(console) = self.console.take() else {
            return;
        };
        let line = console.input.trim().to_string();
        match console.prompt {
            '/' => {
                self.search_term = line;
                self.search_next(true);
            }
            _ => self.dispatch_command(&line),
        }
    }

    pub fn search_next(&mut self, forward: bool) {
        if self.search_term.is_empty() {
            return;
        }
        if let Some(idx) = self.find_match(&self.search_term, forward) {
            self.current_dir_mut().move_to(idx);
        } else {
            self.message = Some(format!("no match: {}", self.search_term));
        }
    }

    fn find_match(&self, term: &str, forward: bool) -> Option<usize> {
        let dir = self.current_dir();
        let n = dir.len();
        if n == 0 {
            return None;
        }
        // ASCII-folded, allocation-free matching — the same semantics as
        // `:filter` (one lowercased needle; entries matched without copies).
        let needle: Vec<u8> = term.bytes().map(|b| b.to_ascii_lowercase()).collect();
        let start = dir.pointer;
        for off in 1..=n {
            let i = if forward {
                (start + off) % n
            } else {
                (start + n - off) % n
            };
            if crate::fs::directory::ci_contains(&dir.entry_at(i).unwrap().name, &needle) {
                return Some(i);
            }
        }
        None
    }

    // ---- command dispatch -------------------------------------------------

    fn dispatch_command(&mut self, line: &str) {
        let mut parts = line.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        match cmd {
            "" => {}
            "q" | "quit" => self.tab_close(),
            "qa" | "quitall" | "quit!" => self.quit = true,
            "cd" => self.cmd_cd(arg),
            "mkdir" => self.cmd_simple(arg, "mkdir", fileops::mkdir),
            "touch" => self.cmd_simple(arg, "touch", fileops::touch),
            "rename" | "mv" => self.cmd_rename(arg),
            "delete" => self.request_delete(),
            "chmod" => self.cmd_chmod(arg),
            "clearbookmarks" => self.clear_bookmarks(),
            "delbookmark" => match arg.chars().next() {
                Some(key) => self.delete_bookmark(key),
                None => self.message = Some("delbookmark: a bookmark key is required".to_string()),
            },
            "filter" => self.cmd_filter(arg),
            "set" => self.cmd_set(arg),
            "search" => {
                self.search_term = arg.to_string();
                self.search_next(true);
            }
            "shell" | "sh" => {
                if !arg.is_empty() {
                    self.pending_run = Some(opener::shell(arg, self.cwd()));
                }
            }
            "open_with" => {
                if !arg.is_empty() {
                    let paths = self.current_dir().selection();
                    self.pending_run = Some(opener::open_with(arg, &paths, self.cwd()));
                }
            }
            "pager" => {
                if let Some(p) = self.selected_path() {
                    self.pending_run = Some(opener::pager(&p, self.cwd()));
                }
            }
            "bulkrename" => self.message = Some("bulkrename: not implemented".to_string()),
            other => self.message = Some(format!("unknown command: {}", other)),
        }
    }

    fn cmd_cd(&mut self, arg: &str) {
        let target = if arg.is_empty() || arg == "~" {
            std::env::var_os("HOME").map(PathBuf::from)
        } else if let Some(rest) = arg.strip_prefix("~/") {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest))
        } else {
            let p = PathBuf::from(arg);
            Some(if p.is_absolute() { p } else { self.cwd().join(p) })
        };
        if let Some(path) = target {
            if path.is_dir() {
                self.cd(path);
            } else {
                self.message = Some(format!("not a directory: {}", arg));
            }
        }
    }

    fn cmd_simple<F>(&mut self, name: &str, verb: &str, f: F)
    where
        F: Fn(&Path) -> std::io::Result<()>,
    {
        if name.is_empty() {
            self.message = Some(format!("{}: name required", verb));
            return;
        }
        let path = self.cwd().join(name);
        match f(&path) {
            Ok(()) => {
                let cwd = self.cwd();
                self.reload_dir(&cwd);
                self.current_dir_mut().select_name(name);
            }
            Err(e) => self.message = Some(format!("{}: {}", verb, e)),
        }
    }

    fn cmd_rename(&mut self, newname: &str) {
        if newname.is_empty() {
            self.message = Some("rename: new name required".to_string());
            return;
        }
        let Some(src) = self.selected_path() else {
            return;
        };
        let dest = self.cwd().join(newname);
        match fileops::rename(&src, &dest) {
            Ok(()) => {
                self.tags.update_path(&src, &dest);
                self.bookmarks.update_path(&src, &dest);
                let cwd = self.cwd();
                self.reload_dir(&cwd);
                if let Some(name) = dest.file_name().map(|s| s.to_string_lossy().into_owned()) {
                    self.current_dir_mut().select_name(&name);
                }
            }
            Err(e) => self.message = Some(format!("rename: {}", e)),
        }
    }

    fn cmd_chmod(&mut self, arg: &str) {
        let Ok(mode) = u32::from_str_radix(arg.trim(), 8) else {
            self.message = Some("chmod: expected octal mode (e.g. 755)".to_string());
            return;
        };
        let targets = self.current_dir().selection();
        let mut errors = 0;
        for t in &targets {
            if fileops::chmod(t, mode).is_err() {
                errors += 1;
            }
        }
        let cwd = self.cwd();
        self.reload_dir(&cwd);
        self.message = Some(if errors == 0 {
            format!("chmod {:o} on {} item(s)", mode, targets.len())
        } else {
            format!("chmod: {} failed", errors)
        });
    }

    fn cmd_filter(&mut self, arg: &str) {
        let settings = self.settings.clone();
        let dir = self.current_dir_mut();
        dir.temporary_filter = if arg.is_empty() {
            None
        } else {
            Some(arg.to_string())
        };
        dir.refilter(&settings);
    }

    fn cmd_set(&mut self, arg: &str) {
        let mut it = arg.splitn(2, char::is_whitespace);
        let name = it.next().unwrap_or("");
        let value = it.next().unwrap_or("").trim();
        let parse_bool = |v: &str| matches!(v, "true" | "True" | "1" | "yes" | "on" | "");
        match name {
            "show_hidden" => {
                self.settings.show_hidden = parse_bool(value);
                let s = self.settings.clone();
                for d in self.dirs.values_mut() {
                    d.refilter(&s);
                }
            }
            "sort" => {
                if let Some(k) = SortKey::from_str(value) {
                    self.set_sort(k);
                }
            }
            "sort_reverse" => {
                self.settings.sort_reverse = parse_bool(value);
                self.refresh_all();
            }
            "sort_directories_first" => {
                self.settings.sort_directories_first = parse_bool(value);
                self.refresh_all();
            }
            "preview_files" => self.settings.preview_files = parse_bool(value),
            "confirm_on_delete" => self.settings.confirm_on_delete = parse_bool(value),
            "draw_borders" => self.settings.draw_borders = parse_bool(value),
            "show_date" | "show_time" => self.settings.show_date = parse_bool(value),
            "time_type" => {
                if let Some(t) = crate::config::TimeType::from_str(value) {
                    self.settings.time_type = t;
                }
            }
            "time_format" => {
                if let Some(f) = crate::config::TimeFormat::from_str(value) {
                    self.settings.time_format = f;
                }
            }
            "size_format" => {
                if let Some(f) = crate::config::SizeFormat::from_str(value) {
                    self.settings.size_format = f;
                }
            }
            "theme" => match crate::theme::Theme::by_name(value) {
                Some(t) => {
                    self.settings.theme = t;
                    self.message = Some(format!("theme: {}", value));
                }
                None => {
                    self.message =
                        Some(format!("unknown theme '{}' ({})", value, crate::theme::Theme::names()))
                }
            },
            other => self.message = Some(format!("set: unknown option {}", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    /// Deleting a directory and recreating it with the same name must not show the
    /// old contents in the preview (the cached listing has to be invalidated).
    #[test]
    fn delete_then_recreate_dir_does_not_show_stale_preview() {
        let base = std::env::temp_dir().join(format!("rr_app_stale_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("test/sub")).unwrap();
        std::fs::write(base.join("test/file"), b"x").unwrap();

        let mut app = App::new(base.clone(), Settings::default());
        app.current_dir_mut().select_name("test");
        app.prepare_view();
        // The preview cached "test" with its two children.
        assert_eq!(app.get_cached(&base.join("test")).map(|d| d.len()), Some(2));

        // Delete "test" through the app: its cache entry must be evicted.
        app.perform_delete(vec![base.join("test")]);
        assert!(app.get_cached(&base.join("test")).is_none());

        // Recreate an empty "test" and re-render.
        std::fs::create_dir_all(base.join("test")).unwrap();
        let cwd = app.cwd();
        app.reload_dir(&cwd);
        app.current_dir_mut().select_name("test");
        app.prepare_view();

        // The preview now reflects the empty recreated directory, not the old one.
        assert_eq!(app.get_cached(&base.join("test")).map(|d| d.len()), Some(0));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The auto-refresh bug: files created/deleted behind rustranger's back
    /// must show up without leaving and re-entering the directory.
    #[test]
    fn external_create_and_delete_refresh_the_view() {
        let base = std::env::temp_dir().join(format!("rr_app_refresh_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a"), b"x").unwrap();

        let mut app = App::new(base.clone(), Settings::default());
        assert_eq!(app.current_dir().len(), 1);

        // Another program creates a file: it must appear on refresh.
        // (Sleeps keep the dir mtime distinct on coarse-timestamp filesystems.)
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(base.join("b"), b"x").unwrap();
        app.refresh_from_disk();
        assert_eq!(app.current_dir().len(), 2);

        // ...and deletes one: it must disappear.
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::remove_file(base.join("a")).unwrap();
        app.refresh_from_disk();
        assert_eq!(app.current_dir().len(), 1);
        assert_eq!(app.current_dir().current().map(|e| e.name.as_str()), Some("b"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Modifying the pointed file's content doesn't bump the directory mtime,
    /// so it is caught by the per-frame re-stat: the row's size and the preview
    /// must both reflect the new content.
    #[test]
    fn modified_selected_file_refreshes_entry_and_preview() {
        use crate::preview::Preview;
        let base = std::env::temp_dir().join(format!("rr_app_mod_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("note.txt"), b"old words").unwrap();

        let mut app = App::new(base.clone(), Settings::default());
        app.current_dir_mut().select_name("note.txt");
        app.prepare_view();
        match app.current_preview() {
            Some(Preview::Text(lines)) => assert!(lines[0].contains("old")),
            _ => panic!("expected a text preview"),
        }

        std::thread::sleep(std::time::Duration::from_millis(30)); // mtime granularity
        let new_body = b"fresh replacement text";
        std::fs::write(base.join("note.txt"), new_body).unwrap();
        app.prepare_view();
        assert_eq!(
            app.current_dir().current().map(|e| e.size),
            Some(new_body.len() as u64),
            "row size must reflect the modified file"
        );
        match app.current_preview() {
            Some(Preview::Text(lines)) => assert!(lines[0].contains("fresh")),
            _ => panic!("expected the refreshed text preview"),
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Entering a FIFO must not launch an opener: the opener's content sniff
    /// would block on `open()` until a writer appears, freezing the TUI with no
    /// way to interrupt it from inside raw mode.
    #[test]
    fn enter_on_a_fifo_does_not_open() {
        let base = std::env::temp_dir().join(format!("rr_app_fifo_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let fifo = base.join("pipe");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);

        let mut app = App::new(base.clone(), Settings::default());
        app.current_dir_mut().select_name("pipe");
        app.enter();
        assert!(app.pending_run.is_none(), "no opener may be launched for a fifo");
        assert!(app.message.as_deref().unwrap_or("").contains("fifo"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A failed copy must say so in the status message, not "complete".
    #[test]
    fn failed_copy_job_reports_the_error() {
        let base = std::env::temp_dir().join(format!("rr_app_joberr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut app = App::new(base.clone(), Settings::default());
        app.jobs.push(copy::start(vec![base.join("does-not-exist")], base.clone(), false));
        for _ in 0..1000 {
            app.tick_jobs();
            if app.jobs.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.jobs.is_empty(), "job must finish");
        let msg = app.message.clone().unwrap_or_default();
        assert!(msg.contains("failed"), "got message: {:?}", msg);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Long sessions must not grow the caches without bound; the cwd (and other
    /// on-screen directories) must never be evicted.
    #[test]
    fn caches_are_evicted_past_their_caps() {
        let base = std::env::temp_dir().join(format!("rr_app_evict_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut app = App::new(base.clone(), Settings::default());
        for i in 0..(DIR_CACHE_MAX + 20) {
            let p = base.join(format!("d{}", i));
            std::fs::create_dir_all(&p).unwrap();
            app.ensure_dir(&p);
        }
        for i in 0..(PREVIEW_CACHE_MAX + 50) {
            app.previews.insert(
                base.join(format!("f{}", i)),
                PreviewSlot { stamp: (0, 0), at: i as u64, preview: Preview::Empty },
            );
        }

        app.prepare_view(); // runs evict_caches
        assert!(app.dirs.len() <= DIR_CACHE_MAX, "dirs: {}", app.dirs.len());
        assert!(app.previews.len() <= PREVIEW_CACHE_MAX, "previews: {}", app.previews.len());
        assert!(app.get_cached(&base).is_some(), "the cwd must never be evicted");

        let _ = std::fs::remove_dir_all(&base);
    }
}
