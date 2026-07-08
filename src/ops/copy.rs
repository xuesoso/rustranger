// Background copy/move worker. A job runs on its own thread and streams Progress
// updates over an mpsc channel; the UI polls them each tick.
// Ported in spirit from ranger/core/loader.py CopyLoader + shutil_generatorized.py.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use super::fileops::get_safe_path;

const CHUNK: usize = 64 * 1024;

/// Minimum interval between streamed progress messages. Without it a fast copy
/// sends (and allocates a label String for) one message per 64 KB chunk —
/// hundreds of thousands for a big file — which the UI, redrawing at ~12 Hz,
/// drains and throws away.
const REPORT_INTERVAL: Duration = Duration::from_millis(50);

/// Rate-limited progress sender for the worker thread.
struct Reporter<'a> {
    tx: &'a Sender<Progress>,
    last: Option<Instant>,
}

impl<'a> Reporter<'a> {
    fn new(tx: &'a Sender<Progress>) -> Reporter<'a> {
        Reporter { tx, last: None }
    }

    /// Send only if `REPORT_INTERVAL` has passed since the last message.
    fn maybe(&mut self, done: u64, total: u64, label: &str) {
        let due = match self.last {
            Some(t) => t.elapsed() >= REPORT_INTERVAL,
            None => true,
        };
        if due {
            self.force(done, total, label);
        }
    }

    /// Send unconditionally (source boundaries, so labels appear promptly).
    fn force(&mut self, done: u64, total: u64, label: &str) {
        self.last = Some(Instant::now());
        let _ = self.tx.send(Progress {
            done,
            total,
            label: label.to_string(),
            finished: false,
            error: None,
        });
    }
}

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
    let mut rep = Reporter::new(tx);

    for src in &sources {
        let name = match src.file_name() {
            Some(n) => n,
            None => continue,
        };
        let target = get_safe_path(&dest.join(name));

        let label = name.to_string_lossy().into_owned();
        rep.force(done, total, &label);

        let result = if cut {
            move_path(src, &target, &mut done, total, &label, &mut rep)
        } else {
            copy_path(src, &target, &mut done, total, &label, &mut rep)
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
    rep: &mut Reporter,
) -> io::Result<()> {
    // Fast path: same-filesystem rename.
    if fs::rename(src, target).is_ok() {
        *done += tree_size(if src.exists() { src } else { target });
        rep.force(*done, total, label);
        return Ok(());
    }
    // Cross-device: copy then delete the source.
    copy_path(src, target, done, total, label, rep)?;
    super::fileops::delete_path(src)
}

fn copy_path(
    src: &Path,
    target: &Path,
    done: &mut u64,
    total: u64,
    label: &str,
    rep: &mut Reporter,
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
            copy_path(&entry.path(), &child_target, done, total, label, rep)?;
        }
        copy_permissions(src, target);
        Ok(())
    } else if ft.is_file() {
        copy_file(src, target, done, total, label, rep)?;
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
    rep: &mut Reporter,
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
        rep.maybe(*done, total, label);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Poll the job to completion (bounded, so a hung worker fails the test).
    fn wait(job: &mut CopyJob) {
        for _ in 0..1000 {
            if !job.poll() {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("copy job did not finish");
    }

    #[test]
    fn copies_a_tree_and_reports_completion() {
        let base = std::env::temp_dir().join(format!("rr_copy_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/sub")).unwrap();
        fs::write(base.join("src/a.txt"), b"hello").unwrap();
        fs::write(base.join("src/sub/b.txt"), b"world").unwrap();
        fs::create_dir_all(base.join("dst")).unwrap();

        let mut job = start(vec![base.join("src")], base.join("dst"), false);
        wait(&mut job);
        assert!(job.progress().error.is_none());
        assert_eq!(fs::read(base.join("dst/src/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(base.join("dst/src/sub/b.txt")).unwrap(), b"world");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_source_reports_an_error() {
        let base = std::env::temp_dir().join(format!("rr_copy_err_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("dst")).unwrap();

        let mut job = start(vec![base.join("does-not-exist")], base.join("dst"), false);
        wait(&mut job);
        assert!(job.progress().error.is_some(), "a failed copy must carry its error");

        let _ = fs::remove_dir_all(&base);
    }
}
