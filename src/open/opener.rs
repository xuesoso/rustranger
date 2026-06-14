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

pub fn open_file(path: &Path, cwd: PathBuf) -> RunRequest {
    if is_text(path) {
        let editor = editor_cmd();
        let mut argv = editor;
        argv.push(path.to_string_lossy().into_owned());
        RunRequest {
            argv,
            block: true,
            cwd,
        }
    } else {
        RunRequest {
            argv: vec![generic_opener().to_string(), path.to_string_lossy().into_owned()],
            block: false,
            cwd,
        }
    }
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
    RunRequest { argv, block, cwd }
}

/// Run a shell command line (`:shell ...`).
pub fn shell(cmdline: &str, cwd: PathBuf) -> RunRequest {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    RunRequest {
        argv: vec![shell, "-c".to_string(), cmdline.to_string()],
        block: true,
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
