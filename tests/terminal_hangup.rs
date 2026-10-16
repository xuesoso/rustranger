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

mod common;

use common::{browse_dir, PtySession};
use std::time::Duration;

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
