// Background copy/move worker. A job runs on its own thread and streams Progress
// updates over an mpsc channel; the UI polls them each tick.
// Ported in spirit from ranger/core/loader.py CopyLoader + shutil_generatorized.py.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use super::fileops::get_safe_path;

const CHUNK: usize = 64 * 1024;

#[derive(Clone)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub label: String,
    pub finished: bool,
    pub error: Option<String>,
}

impl Progress {
    fn empty() -> Progress {
        Progress {
            done: 0,
            total: 0,
            label: String::new(),
            finished: false,
            error: None,
        }
    }
}

pub struct CopyJob {
    rx: Receiver<Progress>,
    pub dest: PathBuf,
    pub cut: bool,
    pub last: Progress,
    _handle: thread::JoinHandle<()>,
}

impl CopyJob {
    /// Drain pending progress messages; returns true while the job is running.
    pub fn poll(&mut self) -> bool {
        while let Ok(p) = self.rx.try_recv() {
            self.last = p;
        }
        !self.last.finished
    }

    pub fn progress(&self) -> &Progress {
        &self.last
    }
}

/// Spawn a copy (or move, when `cut`) of `sources` into directory `dest`.
pub fn start(sources: Vec<PathBuf>, dest: PathBuf, cut: bool) -> CopyJob {
    let (tx, rx) = channel();
    let dest_thread = dest.clone();
    let handle = thread::spawn(move || {
        run(sources, dest_thread, cut, &tx);
    });
    CopyJob {
        rx,
        dest,
        cut,
        last: Progress::empty(),
        _handle: handle,
    }
}

fn run(sources: Vec<PathBuf>, dest: PathBuf, cut: bool, tx: &Sender<Progress>) {
    let total: u64 = sources.iter().map(|s| tree_size(s)).sum();
    let mut done: u64 = 0;
    let mut error: Option<String> = None;

    for src in &sources {
        let name = match src.file_name() {
            Some(n) => n,
            None => continue,
        };
        let target = get_safe_path(&dest.join(name));

        let label = name.to_string_lossy().into_owned();
        let _ = tx.send(Progress {
            done,
            total,
            label: label.clone(),
            finished: false,
            error: None,
        });

        let result = if cut {
            move_path(src, &target, &mut done, total, &label, tx)
        } else {
            copy_path(src, &target, &mut done, total, &label, tx)
        };
        if let Err(e) = result {
            error = Some(format!("{}: {}", label, e));
            break;
        }
    }

    let _ = tx.send(Progress {
        done,
        total,
        label: String::new(),
        finished: true,
        error,
    });
}

fn move_path(
    src: &Path,
    target: &Path,
    done: &mut u64,
    total: u64,
    label: &str,
    tx: &Sender<Progress>,
) -> io::Result<()> {
    // Fast path: same-filesystem rename.
    if fs::rename(src, target).is_ok() {
        *done += tree_size(if src.exists() { src } else { target });
        let _ = tx.send(Progress {
            done: *done,
            total,
            label: label.to_string(),
            finished: false,
            error: None,
        });
        return Ok(());
    }
    // Cross-device: copy then delete the source.
    copy_path(src, target, done, total, label, tx)?;
    super::fileops::delete_path(src)
}

fn copy_path(
    src: &Path,
    target: &Path,
    done: &mut u64,
    total: u64,
    label: &str,
    tx: &Sender<Progress>,
) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();

    if ft.is_symlink() {
        let link_target = fs::read_link(src)?;
        std::os::unix::fs::symlink(link_target, target)?;
        Ok(())
    } else if ft.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let child_target = target.join(entry.file_name());
            copy_path(&entry.path(), &child_target, done, total, label, tx)?;
        }
        copy_permissions(src, target);
        Ok(())
    } else if ft.is_file() {
        copy_file(src, target, done, total, label, tx)?;
        copy_permissions(src, target);
        Ok(())
    } else {
        // Skip sockets/fifos/devices.
        Ok(())
    }
}

fn copy_file(
    src: &Path,
    target: &Path,
    done: &mut u64,
    total: u64,
    label: &str,
    tx: &Sender<Progress>,
) -> io::Result<()> {
    let mut reader = fs::File::open(src)?;
    let mut writer = fs::File::create(target)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        *done += n as u64;
        let _ = tx.send(Progress {
            done: *done,
            total,
            label: label.to_string(),
            finished: false,
            error: None,
        });
    }
    Ok(())
}

fn copy_permissions(src: &Path, target: &Path) {
    if let Ok(meta) = fs::metadata(src) {
        let _ = fs::set_permissions(target, meta.permissions());
    }
}

/// Total byte size of a file or directory tree (best-effort; errors count as 0).
fn tree_size(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if meta.file_type().is_symlink() {
        0
    } else if meta.is_dir() {
        fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        meta.len()
    }
}
