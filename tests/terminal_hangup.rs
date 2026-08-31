//! The terminal going away must end the process, not start an invisible one.
//!
//! Closing a pty master (a terminal window closed, the emulator quit, an ssh
//! link dropped) leaves the slave permanently at EOF. crossterm's Unix event
//! source answers that by calling `read(2)` in a loop that never ends, so
//! `event::poll` never returns — and a TUI that waits inside it is left pinned
//! at 100% CPU with no terminal, deaf to its own SIGTERM flag because its run
//! loop never gets another turn. Nor does closing a master reliably raise
//! SIGHUP, so the signal handler is no defence.
//!
//! These tests drive the real binary on a real pty and assert it dies quickly
//! and quietly. Without `src/tty.rs` the first one hangs until the harness
//! times it out, with the process spinning throughout.

#![cfg(unix)]

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A pty master fd plus the spawned child using its slave as the controlling
/// terminal. Dropping it makes sure nothing is left running.
struct PtySession {
    master: i32,
    child: Child,
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.master >= 0 {
            unsafe { libc::close(self.master) };
        }
    }
}

impl PtySession {
    /// Run `args` on a fresh pty, with a config dir of its own so the developer's
    /// real `config.toml` (and any `[preview]` renderer in it) stays out of it.
    fn spawn(args: &[&str]) -> PtySession {
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

        // A believable window, so the TUI lays out and paints something.
        let ws = libc::winsize { ws_row: 40, ws_col: 120, ws_xpixel: 960, ws_ypixel: 640 };
        unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &ws) };

        let name = unsafe { std::ffi::CStr::from_ptr(libc::ptsname(master)) };
        let slave_path = name.to_str().expect("pts name is utf-8").to_owned();
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&slave_path)
            .expect("open pty slave");

        let cfg = std::env::temp_dir().join(format!("rustranger-hangup-{}", std::process::id()));
        std::fs::create_dir_all(&cfg).expect("scratch config dir");

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
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) < 0 {
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
    fn read_some(&self, budget: Duration) -> Vec<u8> {
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

    /// Close the master: the terminal is gone.
    fn hang_up(&mut self) {
        unsafe { libc::close(self.master) };
        self.master = -1;
    }

    /// How long until the child exits, or `None` if it outlives `limit`.
    fn wait_for_exit(&mut self, limit: Duration) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < limit {
            match self.child.try_wait().expect("try_wait") {
                Some(_) => return Some(start.elapsed()),
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        None
    }

    /// CPU seconds the child has burned so far (Linux only; 0 elsewhere, which
    /// makes the assertions that use it vacuously true rather than wrong).
    fn cpu_secs(&self) -> f64 {
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
fn browse_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rustranger-hangup-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("a.txt"), b"hello").expect("scratch file");
    dir
}

/// The bug, exactly: pull the terminal away and the process must go with it.
#[test]
fn closing_the_terminal_exits_instead_of_spinning() {
    let dir = browse_dir("spin");
    let mut s = PtySession::spawn(&[dir.to_str().unwrap()]);
    let drawn = s.read_some(Duration::from_secs(3));
    assert!(!drawn.is_empty(), "the TUI never painted; test setup is wrong");

    let before = s.cpu_secs();
    s.hang_up();

    let took = s.wait_for_exit(Duration::from_secs(5));
    let burned = s.cpu_secs() - before;
    assert!(
        took.is_some(),
        "still running 5s after the terminal hung up (burned {burned:.2}s of CPU doing it)"
    );
    // Exiting is the point; not melting a core on the way out is the other half.
    assert!(
        burned < 1.0,
        "burned {burned:.2}s of CPU reacting to a hangup that should cost nothing"
    );
}

/// The hangup path must not have cost us the ordinary one: `kill` still works.
#[test]
fn sigterm_still_exits_cleanly() {
    let dir = browse_dir("sigterm");
    let mut s = PtySession::spawn(&[dir.to_str().unwrap()]);
    assert!(!s.read_some(Duration::from_secs(3)).is_empty(), "the TUI never painted");

    unsafe { libc::kill(s.child.id() as libc::pid_t, libc::SIGTERM) };
    assert!(
        s.wait_for_exit(Duration::from_secs(5)).is_some(),
        "SIGTERM no longer ends the process"
    );
}
