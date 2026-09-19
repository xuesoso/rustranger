//! Controlling-terminal liveness: notice when the terminal goes away.
//!
//! When the terminal a TUI is attached to disappears — the window is closed, the
//! emulator quits or crashes, an ssh link drops — the kernel closes the pty
//! master and every read of the slave returns EOF (or `EIO`) from then on.
//! `poll(2)` reports this as `POLLHUP`.
//!
//! crossterm's Unix event source does not handle it. Its inner read loop treats
//! a 0-byte read as "nothing arrived yet", and on any error other than
//! `WouldBlock`/`Interrupted` it falls through the `match` without breaking — so
//! it calls `read(2)` in a tight loop that never ends. `event::poll` then never
//! returns, however short the timeout it was handed. The process is left pinned
//! at 100% CPU with no terminal: invisible, and deaf to the `SIGTERM`/`SIGHUP`
//! flag the run loop checks, because the loop never gets back to the check.
//!
//! The `SIGHUP` handler is no defence — it is half the problem. A hangup *does*
//! signal the foreground process group, and its default action would have killed
//! us outright; catching it (so `kill` and a closed window leave the terminal
//! restored rather than raw) turns that into a flag, and the wedged loop is
//! exactly the thing that can never read the flag. Handling the signal is what
//! lets rustranger outlive its terminal at all.
//!
//! The fix is to own the wait ourselves: [`wait`] blocks in `poll(2)` on the
//! terminal and reports a hangup as a hangup, so crossterm is only ever asked to
//! read a live fd. [`watch_hangup`] covers the microscopic window between that
//! check and the read.
//!
//! Owning the wait costs us the `SIGWINCH` wake-up crossterm's own event source
//! provides, so [`install_resize_wake`] re-adds it through a self-pipe.

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Revents that mean "this terminal is gone for good".
const DEAD: libc::c_short = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;

/// What a [`wait`] ended with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Input is available: crossterm can safely read it.
    Ready,
    /// The window changed size (see [`install_resize_wake`]).
    Resized,
    /// Nothing happened before the timeout.
    Timeout,
    /// The terminal hung up. Never hand the fd to crossterm again — quit.
    Hangup,
}

/// The fd crossterm reads events from: stdin when it is a tty, else `/dev/tty`.
/// Opened once and deliberately leaked — it lives as long as the process.
fn tty_fd() -> Option<RawFd> {
    static FD: OnceLock<Option<RawFd>> = OnceLock::new();
    *FD.get_or_init(|| {
        if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
            return Some(libc::STDIN_FILENO);
        }
        let fd = unsafe { libc::open(c"/dev/tty".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        (fd >= 0).then_some(fd)
    })
}

/// Read end of the `SIGWINCH` self-pipe, or -1 before [`install_resize_wake`].
static WINCH_R: AtomicI32 = AtomicI32::new(-1);
/// Write end, written from the signal handler.
static WINCH_W: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_winch(_sig: libc::c_int) {
    let fd = WINCH_W.load(Ordering::Relaxed);
    if fd >= 0 {
        // write(2) on a pipe is async-signal-safe. A full pipe (EAGAIN) means a
        // wake-up is already queued, which is all we needed anyway.
        let byte = 1u8;
        unsafe { libc::write(fd, std::ptr::addr_of!(byte).cast(), 1) };
    }
}

/// Make window resizes wake [`wait`], via the classic self-pipe trick.
///
/// This replaces crossterm's own `SIGWINCH` handler, so call it *after* the
/// crossterm event source exists (one `event::poll` is enough to build it) —
/// otherwise crossterm's lazy registration would clobber ours instead. Losing
/// crossterm's handler is harmless here: the run loop re-reads the terminal size
/// every frame and ignores `Event::Resize`, it just needs to be woken.
pub fn install_resize_wake() {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: fds is a valid 2-element array; pipe only writes into it.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    for fd in fds {
        // Non-blocking so the handler can never stall on a full pipe, and
        // close-on-exec so children don't inherit it.
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    WINCH_R.store(fds[0], Ordering::Relaxed);
    WINCH_W.store(fds[1], Ordering::Relaxed);
    let handler = on_winch as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe { libc::signal(libc::SIGWINCH, handler) };
}

/// Drain the resize pipe: one wake-up per `wait`, not one per signal.
fn drain_winch(fd: RawFd) {
    let mut buf = [0u8; 64];
    // SAFETY: buf is a valid writable buffer; the fd is non-blocking.
    while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}

/// Wait up to `timeout` for terminal input, a resize, or a hangup.
///
/// Only [`Wait::Ready`] means crossterm may read. With no terminal to watch
/// (piped stdin and no `/dev/tty`) this reports `Ready` so the caller falls back
/// to crossterm's own blocking wait.
pub fn wait(timeout: Duration) -> Wait {
    let Some(tty) = tty_fd() else {
        return Wait::Ready;
    };
    let winch = WINCH_R.load(Ordering::Relaxed);
    let mut fds = [
        libc::pollfd { fd: tty, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: winch, events: libc::POLLIN, revents: 0 },
    ];
    let n = if winch >= 0 { 2 } else { 1 };
    let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    // SAFETY: fds holds n valid, initialised pollfds; poll only reads/writes them.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), n, ms) };
    if rc < 0 {
        // EINTR (or anything else): report a timeout so the caller loops and
        // re-checks its own quit flags, exactly as a quiet tick would.
        return Wait::Timeout;
    }
    // A hangup outranks pending input: the bytes are moot, and reading past them
    // is what wedges crossterm.
    if fds[0].revents & DEAD != 0 {
        return Wait::Hangup;
    }
    if n == 2 && fds[1].revents & libc::POLLIN != 0 {
        drain_winch(winch);
        return Wait::Resized;
    }
    if fds[0].revents & libc::POLLIN != 0 {
        return Wait::Ready;
    }
    Wait::Timeout
}

/// The terminal's size in cells, from the fd we already hold.
///
/// `crossterm::terminal::size()` opens and closes `/dev/tty` on every call. The
/// run loop asks once per frame — and once per keystroke while scrolling — so
/// that is three syscalls where one will do. `None` when there is no terminal
/// (piped stdin, no `/dev/tty`), leaving the caller its own fallback.
pub fn size() -> Option<(u16, u16)> {
    let fd = tty_fd()?;
    // SAFETY: ws is a plain POD struct; TIOCGWINSZ only writes into it.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    (rc == 0 && ws.ws_col > 0 && ws.ws_row > 0).then_some((ws.ws_col, ws.ws_row))
}

/// Has the controlling terminal hung up? Non-blocking, and never consumes the
/// input crossterm needs — `poll(2)` only reports readiness, it does not read.
///
/// **`events` must not be empty.** POSIX says `POLLHUP`/`POLLERR`/`POLLNVAL` are
/// reported regardless of what was requested, and Linux does exactly that — but
/// macOS/BSD report nothing at all for a `poll` that asked for nothing, even on
/// a pty whose master is closed. Asking for `POLLIN` (which a dead terminal sets
/// anyway, at EOF) is what makes this work on both. Only the `DEAD` bits are
/// consulted, so pending keystrokes never read as a hangup.
pub fn hung_up() -> bool {
    let Some(fd) = tty_fd() else {
        return false;
    };
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: pfd is a valid, initialised pollfd; poll only reads/writes it.
    let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
    rc > 0 && pfd.revents & DEAD != 0
}

/// Start the hangup watchdog.
///
/// The thread blocks in `poll(2)` — costing nothing while the terminal is alive —
/// and wakes the instant it hangs up. It sets the caller's terminate flag so the
/// run loop shuts down normally (restoring the screen, dropping its children),
/// then gives it `grace` to do so. If the loop is wedged anyway — inside
/// crossterm's read loop, or blocked writing to a terminal nobody will ever
/// drain — it will never observe the flag, so we exit the process outright
/// rather than leave a detached, CPU-burning orphan behind.
pub fn watch_hangup(
    grace: Duration,
    on_hangup: impl Fn() + Send + 'static,
    before_force_exit: impl Fn() + Send + 'static,
) {
    if tty_fd().is_none() {
        return;
    }
    std::thread::spawn(move || {
        loop {
            // -1 blocks until the fd has something to say. `POLLIN` must be
            // requested: a poll that asks for nothing is never answered on
            // macOS/BSD, which would leave this watchdog asleep forever —
            // exactly when it is the last thing standing (see `hung_up`).
            let fd = tty_fd().unwrap();
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: pfd is a valid, initialised pollfd.
            let rc = unsafe { libc::poll(&mut pfd, 1, -1) };
            if rc > 0 && pfd.revents & DEAD != 0 {
                break;
            }
            // Woken by ordinary input, which we deliberately do not read — it
            // belongs to the run loop. Until it is consumed the fd stays ready
            // and poll would return at once, so pause briefly rather than spin
            // a core while the user holds a key down.
            if rc > 0 {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        on_hangup();
        std::thread::sleep(grace);
        // Still running: the main thread is stuck. Take any child processes with
        // us — `_exit` runs no destructors, so nothing else will — and go.
        before_force_exit();
        // SAFETY: _exit is async-signal-safe and performs no unwinding.
        unsafe { libc::_exit(0) };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no terminal at all (the test harness has no tty on stdin, and may
    /// have no /dev/tty), the helpers must degrade quietly rather than report a
    /// phantom hangup and quit a perfectly healthy session.
    #[test]
    fn no_terminal_is_not_a_hangup() {
        if tty_fd().is_none() {
            assert!(!hung_up());
            assert_eq!(wait(Duration::ZERO), Wait::Ready);
        }
    }

    /// A pipe whose write end is closed is the shape of a dead terminal. It is
    /// the signal the watchdog and `wait` are built on, so pin down the contract
    /// they actually rely on: **the poll must request `POLLIN`**.
    ///
    /// POSIX says `POLLHUP` is reported whatever you asked for, and Linux obliges,
    /// but macOS/BSD answer a poll that requested nothing with nothing — verified
    /// here on both, by asserting only the armed form. An unarmed poll is left
    /// untested precisely because its answer is platform-dependent; every caller
    /// in this module arms `POLLIN` so none of them depend on it.
    #[test]
    fn closed_pipe_polls_as_hangup_when_armed() {
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { libc::close(fds[1]) }; // writer gone: the "terminal" hung up
        let mut pfd = libc::pollfd { fd: fds[0], events: libc::POLLIN, revents: 0 };
        let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
        assert!(rc > 0 && pfd.revents & DEAD != 0, "revents={}", pfd.revents);
        unsafe { libc::close(fds[0]) };
    }

    /// Guard the fix itself: no poll in this module may leave `events` empty, or
    /// it silently stops noticing hangups on macOS while still passing on Linux.
    #[test]
    fn every_poll_in_this_module_arms_events() {
        let src = include_str!("tty.rs");
        for (n, line) in src.lines().enumerate() {
            // Leading space so this doesn't also match the `revents: 0` field,
            // which ends in the very same characters.
            let is_decl = line.contains("libc::pollfd {") && line.contains("events:");
            if is_decl && line.contains(" events: 0") {
                panic!("tty.rs:{}: poll with empty events is unanswered on macOS: {}", n + 1, line.trim());
            }
        }
    }
}
