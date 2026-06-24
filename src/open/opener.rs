// "rifle-lite": decide how to open a file. No rifle.conf / external `file` needed.
// Text-like files open in $EDITOR; everything else via xdg-open (or `open` on mac).
// Mirrors the role of ranger/ext/rifle.py + core/runner.py, drastically simplified.

use std::path::{Path, PathBuf};

use super::RunRequest;

/// Extensions we consider editable text (open in $EDITOR).
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "markdown", "rst", "org", "tex", "log", "conf", "cfg", "ini", "toml", "yaml",
    "yml", "json", "xml", "html", "htm", "css", "scss", "js", "ts", "jsx", "tsx", "c", "h", "cpp",
    "hpp", "cc", "cxx", "rs", "go", "py", "rb", "pl", "pm", "php", "java", "kt", "scala", "swift",
    "sh", "bash", "zsh", "fish", "vim", "lua", "sql", "csv", "tsv", "make", "mk", "cmake",
    "gitignore", "dockerfile", "env", "diff", "patch",
];

/// Filenames (no extension) that are text.
const TEXT_NAMES: &[&str] = &[
    "readme", "license", "makefile", "dockerfile", "changelog", "authors", "todo", "copying",
    ".bashrc", ".zshrc", ".profile", ".gitconfig", ".vimrc",
];

/// Decide how to open `path`. A user-configured `[open]` entry for the file's
/// extension wins; otherwise text files open in `$EDITOR` and everything else
/// goes to the platform opener (`xdg-open`, or `open` on macOS).
pub fn open_file(path: &Path, cwd: PathBuf, openers: &[(String, String)]) -> RunRequest {
    if let Some(cmd) = user_opener(path, openers) {
        return build_command(cmd, path, cwd);
    }
    if is_text(path) {
        let editor = editor_cmd();
        let mut argv = editor;
        argv.push(path.to_string_lossy().into_owned());
        RunRequest {
            argv,
            block: true,
            fullscreen: true,
            cwd,
        }
    } else {
        RunRequest {
            argv: vec![generic_opener().to_string(), path.to_string_lossy().into_owned()],
            block: false,
            fullscreen: false,
            cwd,
        }
    }
}

/// Compression suffixes that are "peeled" during opener lookup, so a `.csv.gz`
/// can fall back to the `.csv` opener.
const COMPRESSION_EXTS: &[&str] = &["gz", "bz2", "xz", "zst", "lz4", "lzma", "lz", "z", "br"];

/// The configured command for `path`, if any. Extension candidates are tried
/// most- to least-specific (see `ext_candidates`), case-insensitively.
fn user_opener<'a>(path: &Path, openers: &'a [(String, String)]) -> Option<&'a str> {
    for cand in ext_candidates(path) {
        if let Some((_, cmd)) = openers.iter().find(|(e, _)| *e == cand) {
            return Some(cmd.as_str());
        }
    }
    None
}

/// Extension candidates for opener lookup, most-specific first. The dotted
/// suffixes (longest → shortest) come first so an explicit compound mapping wins;
/// then, when the file ends in a known compression suffix, the inner extension is
/// inserted so a `.csv.gz` uses the `.csv` opener. Examples:
///   "data.csv.gz"  -> ["csv.gz", "csv", "gz"]
///   "a.b.tsv.zst"  -> ["b.tsv.zst", "tsv.zst", "tsv", "zst"]
///   "data.csv"     -> ["csv"]
fn ext_candidates(path: &Path) -> Vec<String> {
    let name = match path.file_name() {
        Some(n) => n.to_string_lossy().to_lowercase(),
        None => return Vec::new(),
    };
    // A leading dot (hidden file) is part of the stem, not an extension dot.
    let parts: Vec<&str> = name.trim_start_matches('.').split('.').collect();
    if parts.len() < 2 {
        return Vec::new(); // no extension
    }
    let exts = &parts[1..]; // drop the stem, e.g. ["csv", "gz"]
    let mut cands: Vec<String> = (0..exts.len()).map(|i| exts[i..].join(".")).collect();
    // Peel a known compression suffix: a content-type opener (e.g. "csv") should
    // win over the bare compression opener ("gz") but lose to an explicit compound.
    if exts.len() >= 2 && COMPRESSION_EXTS.contains(&exts[exts.len() - 1]) {
        let inner = exts[exts.len() - 2].to_string();
        if !cands.contains(&inner) {
            let pos = cands.len() - 1; // just before the bare compression candidate
            cands.insert(pos, inner);
        }
    }
    cands
}

/// Build a run request from a user command template. Tokens are split on
/// whitespace (no shell is involved, so this behaves identically on macOS and
/// Linux). Conventions:
///   - a trailing `&` token runs the program detached and keeps the TUI up (for
///     GUI apps); without it the program runs in the foreground, suspending the
///     TUI (correct for terminal apps like editors, pagers, or rustranger);
///   - a `{}` token is replaced by the file path; if there is no `{}`, the path
///     is appended as the final argument.
fn build_command(cmd: &str, path: &Path, cwd: PathBuf) -> RunRequest {
    let mut tokens: Vec<String> = cmd.split_whitespace().map(String::from).collect();
    let block = if tokens.last().map(|t| t == "&").unwrap_or(false) {
        tokens.pop();
        false
    } else {
        true
    };

    let p = path.to_string_lossy().into_owned();
    let mut argv: Vec<String> = Vec::with_capacity(tokens.len() + 1);
    let mut substituted = false;
    for t in tokens {
        if t == "{}" {
            argv.push(p.clone());
            substituted = true;
        } else {
            argv.push(t);
        }
    }
    if !substituted {
        argv.push(p);
    }
    // A foreground (blocking) opener is assumed to be a full-screen terminal app
    // (editor/pager/TUI viewer) — the common case — so the alternate screen is
    // kept across the handoff to avoid the default-background flash. Detached (`&`)
    // GUI apps don't suspend the TUI at all, so the flag is moot for them.
    RunRequest { argv, block, fullscreen: block, cwd }
}

/// Open a file (or selection) with an explicitly named program.
pub fn open_with(program: &str, paths: &[PathBuf], cwd: PathBuf) -> RunRequest {
    // Split the program string so "open_with vim -p" works.
    let mut argv: Vec<String> = program.split_whitespace().map(String::from).collect();
    for p in paths {
        argv.push(p.to_string_lossy().into_owned());
    }
    // Heuristic: terminal editors/pagers block; assume named programs block unless
    // they look like a GUI opener.
    let block = !matches!(argv.first().map(String::as_str), Some("xdg-open") | Some("open"));
    RunRequest { argv, block, fullscreen: block, cwd }
}

/// Run a shell command line (`:shell ...`).
pub fn shell(cmdline: &str, cwd: PathBuf) -> RunRequest {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    RunRequest {
        argv: vec![shell, "-c".to_string(), cmdline.to_string()],
        block: true,
        // Inline: leave the alternate screen so command output lands on the normal
        // screen (a `:shell` line may just print text rather than draw a UI).
        fullscreen: false,
        cwd,
    }
}

pub fn pager(path: &Path, cwd: PathBuf) -> RunRequest {
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
    let mut argv: Vec<String> = pager.split_whitespace().map(String::from).collect();
    argv.push(path.to_string_lossy().into_owned());
    RunRequest {
        argv,
        block: true,
        fullscreen: true,
        cwd,
    }
}

fn editor_cmd() -> Vec<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    editor.split_whitespace().map(String::from).collect()
}

fn generic_opener() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

fn is_text(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if TEXT_NAMES.contains(&name.as_str()) {
        return true;
    }
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        if TEXT_EXTS.contains(&ext.as_str()) {
            return true;
        }
    }
    // Fall back to a content sniff for extensionless files.
    if path.extension().is_none() {
        return sniff_text(path);
    }
    false
}

fn sniff_text(path: &Path) -> bool {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut buf = [0u8; 256];
        if let Ok(n) = f.read(&mut buf) {
            if n == 0 {
                return true;
            }
            return !buf[..n].contains(&0);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd() -> PathBuf {
        PathBuf::from("/tmp")
    }

    #[test]
    fn build_command_appends_path_by_default() {
        let r = build_command("rustranger", Path::new("/a/b.csv"), cwd());
        assert_eq!(r.argv, vec!["rustranger", "/a/b.csv"]);
        assert!(r.block, "terminal app runs in the foreground");
    }

    #[test]
    fn build_command_substitutes_placeholder() {
        let r = build_command("unzip -l {}", Path::new("/a/b.zip"), cwd());
        assert_eq!(r.argv, vec!["unzip", "-l", "/a/b.zip"]);
        assert!(r.block);
    }

    #[test]
    fn build_command_trailing_amp_forks() {
        let r = build_command("zathura &", Path::new("/a/b.pdf"), cwd());
        assert_eq!(r.argv, vec!["zathura", "/a/b.pdf"]);
        assert!(!r.block, "trailing & detaches (GUI app), TUI stays up");
    }

    /// The `fullscreen` flag drives whether the main loop keeps the alternate screen
    /// across the handoff (no default-background flash). Foreground terminal apps —
    /// editors, pagers, foreground `[open]` commands — keep it; inline `:shell` does
    /// not (so its output reaches the normal screen); detached GUI apps never block.
    #[test]
    fn fullscreen_flag_tracks_terminal_takeover() {
        // $EDITOR text open: blocking + full-screen.
        let ed = open_file(Path::new("/x/notes.txt"), cwd(), &[]);
        assert!(ed.block && ed.fullscreen, "editor takes over the screen");

        // Foreground [open] command: blocking + full-screen.
        let fg = build_command("rustidata", Path::new("/x/data.csv"), cwd());
        assert!(fg.block && fg.fullscreen);

        // Detached [open] (`&`): not blocking, so fullscreen is moot (and false).
        let gui = build_command("zathura &", Path::new("/x/a.pdf"), cwd());
        assert!(!gui.block && !gui.fullscreen);

        // Pager: blocking + full-screen.
        let pg = pager(Path::new("/x/a.log"), cwd());
        assert!(pg.block && pg.fullscreen);

        // :shell: blocking but NOT full-screen (output goes to the normal screen).
        let sh = shell("echo hi", cwd());
        assert!(sh.block && !sh.fullscreen);
    }

    #[test]
    fn user_opener_wins_over_builtin() {
        let openers = vec![("csv".to_string(), "rustranger".to_string())];
        // .csv is in the built-in text list, but the user mapping takes priority.
        let r = open_file(Path::new("/x/data.csv"), cwd(), &openers);
        assert_eq!(r.argv[0], "rustranger");
        assert_eq!(r.argv.last().unwrap(), "/x/data.csv");
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        let openers = vec![("csv".to_string(), "rustranger".to_string())];
        let r = open_file(Path::new("/x/DATA.CSV"), cwd(), &openers);
        assert_eq!(r.argv[0], "rustranger");
    }

    #[test]
    fn falls_back_to_generic_opener_for_unmapped_binary() {
        let r = open_file(Path::new("/x/pic.png"), cwd(), &[]);
        assert!(matches!(r.argv[0].as_str(), "xdg-open" | "open"));
        assert!(!r.block);
    }

    #[test]
    fn ext_candidates_peels_compression() {
        assert_eq!(ext_candidates(Path::new("/x/data.csv.gz")), ["csv.gz", "csv", "gz"]);
        assert_eq!(ext_candidates(Path::new("/x/a.b.tsv.zst")), ["b.tsv.zst", "tsv.zst", "tsv", "zst"]);
        assert_eq!(ext_candidates(Path::new("/x/data.csv")), ["csv"]);
        // Not a compression suffix => no peel, just the dotted suffixes.
        assert_eq!(ext_candidates(Path::new("/x/a.foo.bar")), ["foo.bar", "bar"]);
        assert!(ext_candidates(Path::new("/x/noext")).is_empty());
    }

    #[test]
    fn compressed_file_uses_inner_extension_opener() {
        let openers = vec![
            ("csv".to_string(), "rustidata".to_string()),
            ("tsv".to_string(), "rustidata".to_string()),
        ];
        let r = open_file(Path::new("/x/data.csv.gz"), cwd(), &openers);
        assert_eq!(r.argv[0], "rustidata");
        assert_eq!(r.argv.last().unwrap(), "/x/data.csv.gz");
        let r = open_file(Path::new("/x/data.tsv.gz"), cwd(), &openers);
        assert_eq!(r.argv[0], "rustidata");
    }

    #[test]
    fn explicit_compound_wins_over_peeled_inner() {
        let openers = vec![
            ("csv".to_string(), "rustidata".to_string()),
            ("csv.gz".to_string(), "zcat-viewer".to_string()),
        ];
        // The full compound "csv.gz" is more specific than the peeled "csv".
        let r = open_file(Path::new("/x/data.csv.gz"), cwd(), &openers);
        assert_eq!(r.argv[0], "zcat-viewer");
    }
}
