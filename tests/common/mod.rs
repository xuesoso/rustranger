//! A real pty to run the rustranger binary on, for tests that need it to
//! believe it is talking to a terminal. The test plays the terminal: it reads
//! what rustranger draws and writes what a user (or the terminal) would send.

#![allow(dead_code)] // each test binary uses its own subset

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A pty master fd plus the spawned child using its slave as the controlling
/// terminal. Dropping it makes sure nothing is left running.
pub struct PtySession {
    pub master: i32,
    pub child: Child,
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Close the master first. An exiting child's last close of its terminal
        // waits for unread output to drain (macOS), and nobody reads the master
        // any more — so waiting while still holding it open never returns.
        if self.master >= 0 {
            unsafe { libc::close(self.master) };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PtySession {
    /// Run `args` on a fresh pty, with a config dir of its own so the developer's
    /// real `config.toml` (and any `[preview]` renderer in it) stays out of it.
    pub fn spawn(args: &[&str]) -> PtySession {
        PtySession::spawn_with_config(args, None)
    }

    /// [`spawn`](Self::spawn) with `config` as the session's `config.toml`.
    pub fn spawn_with_config(args: &[&str], config: Option<&str>) -> PtySession {
        // `ptsname` answers from a single static buffer, so two tests allocating
        // ptys at once can each be handed the other's name — and then closing a
        // master hangs up somebody else's child, not ours. Allocate one at a time.
        static PTY_ALLOC: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = PTY_ALLOC.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner());

        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        assert!(master >= 0, "posix_openpt: {}", std::io::Error::last_os_error());
        // Close-on-exec, or the child inherits the master and the pty can never
        // hang up: our close leaves the child holding the last reference to the
        // terminal it is supposedly losing.
        unsafe { libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) };
        assert_eq!(unsafe { libc::grantpt(master) }, 0);
        assert_eq!(unsafe { libc::unlockpt(master) }, 0);

        let name = unsafe { std::ffi::CStr::from_ptr(libc::ptsname(master)) };
        let slave_path = name.to_str().expect("pts name is utf-8").to_owned();
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&slave_path)
            .expect("open pty slave");
        // A believable window, so the TUI lays out and paints something. Set
        // through the opened slave: on macOS a size set on the master before the
        // slave is first opened doesn't stick, leaving a 0x0 window.
        let ws = libc::winsize { ws_row: 40, ws_col: 120, ws_xpixel: 960, ws_ypixel: 640 };
        unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ, &ws) };

        // One config dir per session: tests in a binary run in parallel.
        static SESSION: AtomicUsize = AtomicUsize::new(0);
        let n = SESSION.fetch_add(1, Ordering::Relaxed);
        let cfg = std::env::temp_dir().join(format!("rustranger-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(cfg.join("rustranger")).expect("scratch config dir");
        if let Some(text) = config {
            std::fs::write(cfg.join("rustranger").join("config.toml"), text).expect("write config");
        }

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rustranger"));
        cmd.args(args)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .env("TERM", "xterm-256color")
            .env("XDG_CONFIG_HOME", &cfg)
            .env_remove("TMUX");
        // Give the child its own session with this pty as the controlling
        // terminal — the shape a terminal emulator sets up, and the one where a
        // hangup is meaningful.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // TIOCSCTTY is u32 on macOS/BSD but c_ulong on Linux; cast so this
                // compiles on both.
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn rustranger on a pty");
        // The parent must not keep the slave open: while any writer exists the
        // master's close is not a hangup at all.
        drop(slave);
        PtySession { master, child }
    }

    /// Read whatever the TUI paints, up to `budget`, so we know it reached its
    /// event loop before we pull the terminal out from under it.
    pub fn read_some(&self, budget: Duration) -> Vec<u8> {
        let mut got = Vec::new();
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            let mut pfd = libc::pollfd { fd: self.master, events: libc::POLLIN, revents: 0 };
            let rc = unsafe { libc::poll(&mut pfd, 1, 100) };
            if rc <= 0 {
                if !got.is_empty() {
                    break;
                }
                continue;
            }
            let mut file = unsafe { std::fs::File::from_raw_fd(self.master) };
            let mut buf = [0u8; 8192];
            let n = file.read(&mut buf).unwrap_or(0);
            std::mem::forget(file); // the fd stays ours
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        got
    }

    /// Send `bytes` as if typed at (or answered by) the terminal.
    pub fn write(&self, bytes: &[u8]) {
        let mut file = unsafe { std::fs::File::from_raw_fd(self.master) };
        let res = file.write_all(bytes);
        std::mem::forget(file); // the fd stays ours
        res.expect("write to pty master");
    }

    /// Read output until it contains `needle` (returning everything read), or
    /// `None` once `budget` passes without it.
    pub fn read_until(&self, needle: &[u8], budget: Duration) -> Option<Vec<u8>> {
        let deadline = Instant::now() + budget;
        let mut got = Vec::new();
        while Instant::now() < deadline {
            got.extend(self.read_some(Duration::from_millis(50)));
            if got.windows(needle.len()).any(|w| w == needle) {
                return Some(got);
            }
        }
        None
    }

    /// Close the master: the terminal is gone.
    pub fn hang_up(&mut self) {
        unsafe { libc::close(self.master) };
        self.master = -1;
    }

    /// How long until the child exits, or `None` if it outlives `limit`.
    pub fn wait_for_exit(&mut self, limit: Duration) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < limit {
            match self.child.try_wait().expect("try_wait") {
                Some(_) => return Some(start.elapsed()),
                // Keep reading what it writes, as a real terminal would: its
                // shutdown output (leaving the alternate screen) otherwise fills
                // the pty and the exit blocks on a write nobody will drain.
                None if self.master >= 0 => {
                    self.read_some(Duration::from_millis(20));
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        None
    }

    /// CPU seconds the child has burned so far (Linux only; 0 elsewhere, which
    /// makes the assertions that use it vacuously true rather than wrong).
    pub fn cpu_secs(&self) -> f64 {
        let path = format!("/proc/{}/stat", self.child.id());
        let Ok(stat) = std::fs::read_to_string(path) else {
            return 0.0;
        };
        // Fields after the (possibly paren-laden) comm: utime is 14th, stime 15th.
        let Some(rest) = stat.rsplit_once(')') else { return 0.0 };
        let f: Vec<&str> = rest.1.split_whitespace().collect();
        let ticks: f64 = match (f.get(11), f.get(12)) {
            (Some(u), Some(s)) => u.parse().unwrap_or(0.0) + s.parse::<f64>().unwrap_or(0.0),
            _ => return 0.0,
        };
        ticks / 100.0 // sysconf(_SC_CLK_TCK) is 100 on every platform we ship to
    }
}

/// A small directory to browse, so the test doesn't depend on what /tmp holds.
pub fn browse_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rustranger-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("a.txt"), b"hello").expect("scratch file");
    dir
}
