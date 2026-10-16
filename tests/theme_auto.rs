//! `theme = "auto"` against a terminal played by the test: rustranger asks for
//! the background color (`OSC 11`), subscribes to scheme changes (mode 2031),
//! and must switch between `light_theme` and `dark_theme` as the answers change
//! — without ever mistaking a reply for keystrokes, and without leaving a reply
//! behind for whatever reads the terminal after it.

#![cfg(unix)]

mod common;

use common::{browse_dir, PtySession};
use std::time::Duration;

const QUERY: &[u8] = b"\x1b]11;?";
const SUBSCRIBE: &[u8] = b"\x1b[?2031h";
const UNSUBSCRIBE: &[u8] = b"\x1b[?2031l";
/// What a terminal sends back: the background, then the DA1 answer.
const LIGHT_REPLY: &[u8] = b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;22c";
const DARK_REPLY: &[u8] = b"\x1b]11;rgb:1f1f/1f1f/2828\x07\x1b[?62;22c";
/// The two themes' backgrounds as rustranger paints them (SGR 48;2).
const LIGHT_BG: &[u8] = b"48;2;251;241;199m"; // gruvbox-light #fbf1c7
const DARK_BG: &[u8] = b"48;2;40;40;40m"; // gruvbox-dark #282828

const AUTO: &str = "[settings]\ntheme = \"auto\"\nlight_theme = \"gruvbox-light\"\ndark_theme = \"gruvbox-dark\"\n";
const WAIT: Duration = Duration::from_secs(3);

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn start_auto(tag: &str) -> PtySession {
    let dir = browse_dir(tag);
    PtySession::spawn_with_config(&[dir.to_str().unwrap()], Some(AUTO))
}

#[test]
fn follows_the_terminal_background() {
    let mut s = start_auto("follow");

    // Startup: asked before the first paint, which then comes out light.
    s.read_until(QUERY, WAIT).expect("no startup background query");
    s.write(LIGHT_REPLY);
    let first = s.read_until(LIGHT_BG, WAIT).expect("never painted the light theme");
    assert!(!contains(&first, DARK_BG), "first paint used the dark theme before switching");
    // Subscribed to scheme changes along the way (just before that paint).
    assert!(contains(&first, SUBSCRIBE), "never subscribed to scheme changes");

    // The terminal announces a scheme change: rustranger re-asks at once and
    // repaints dark — no key press needed.
    s.read_some(Duration::from_millis(200)); // let the startup round settle
    s.write(b"\x1b[?997;1n");
    s.read_until(QUERY, WAIT).expect("a scheme notification didn't trigger a query");
    s.write(DARK_REPLY);
    s.read_until(DARK_BG, WAIT).expect("didn't switch to the dark theme");

    // A terminal that never announces anything: a key press after a pause
    // re-checks (once per second at most), and the answer switches back.
    std::thread::sleep(Duration::from_millis(1100));
    s.write(b"j");
    s.read_until(QUERY, WAIT).expect("a key press after a pause didn't re-check");
    s.write(LIGHT_REPLY);
    s.read_until(LIGHT_BG, WAIT).expect("didn't switch back to the light theme");

    // A key arriving in the same read as a reply is still a key: `Q` quits.
    std::thread::sleep(Duration::from_millis(1100));
    s.write(b"j");
    s.read_until(QUERY, WAIT).expect("no re-check");
    let mut burst = LIGHT_REPLY.to_vec();
    burst.extend_from_slice(b"Q");
    s.write(&burst);
    let tail = s.read_until(UNSUBSCRIBE, WAIT).expect("never unsubscribed on the way out");
    assert!(!tail.is_empty());
    assert!(s.wait_for_exit(WAIT).is_some(), "the Q after a reply was swallowed");
}

/// Quitting with an answer still on its way waits for it, so it isn't left for
/// the shell to print at its prompt — but only briefly.
#[test]
fn quitting_collects_a_reply_still_in_flight() {
    let mut s = start_auto("inflight");
    s.read_until(QUERY, WAIT).expect("no startup query");
    s.write(LIGHT_REPLY);
    s.read_until(LIGHT_BG, WAIT).expect("no paint");

    std::thread::sleep(Duration::from_millis(1100));
    s.write(b"j");
    s.read_until(QUERY, WAIT).expect("no re-check");
    // Quit before answering: rustranger must hold on for the reply...
    s.write(b"Q");
    s.read_some(Duration::from_millis(100));
    assert!(s.child.try_wait().unwrap().is_none(), "left with a reply still in flight");
    // ...and go as soon as it lands.
    s.write(LIGHT_REPLY);
    let took = s.wait_for_exit(WAIT).expect("didn't exit after the reply arrived");
    assert!(took < Duration::from_secs(1), "slow to exit after the reply: {took:?}");
}

/// The same, when the terminal never answers: the wait is bounded.
#[test]
fn an_unanswered_query_never_holds_up_quitting_for_long() {
    let mut s = start_auto("silent");
    s.read_until(QUERY, WAIT).expect("no startup query");
    // Never answer anything. The first paint still happens (after the bounded
    // startup wait), in the dark theme by default.
    s.read_until(DARK_BG, WAIT).expect("no paint without an answer");
    s.write(b"Q");
    let took = s.wait_for_exit(WAIT).expect("hung waiting for an answer that never came");
    assert!(took < Duration::from_secs(1), "slow to exit: {took:?}");
}

/// A fixed theme costs nothing: no queries, no subscription, keys as before.
#[test]
fn a_fixed_theme_never_queries() {
    let dir = browse_dir("fixed");
    let mut s = PtySession::spawn_with_config(
        &[dir.to_str().unwrap()],
        Some("[settings]\ntheme = \"gruvbox-dark\"\n"),
    );
    let mut out = s.read_until(DARK_BG, WAIT).expect("no paint");
    s.write(b"jkjk");
    out.extend(s.read_some(Duration::from_millis(300)));
    assert!(!contains(&out, QUERY), "queried the background with a fixed theme");
    assert!(!contains(&out, SUBSCRIBE), "subscribed to scheme changes with a fixed theme");
    s.write(b"Q");
    assert!(s.wait_for_exit(WAIT).is_some(), "Q didn't quit");
}

/// A theme switch re-renders the image preview against the new background:
/// the renderer is called again with the new `%b` (which `--theme auto` in
/// folio, and `preview_theme = "auto"`, key off).
#[test]
fn image_preview_rerenders_with_the_new_background() {
    use std::os::unix::fs::PermissionsExt;
    let dir = browse_dir("image");
    let _ = std::fs::remove_file(dir.join("a.txt")); // leave the image selected
    std::fs::write(dir.join("a.png"), b"not really a png").unwrap();
    // Stand-in renderer: logs the background and theme it was given.
    let log = dir.join("renders.log");
    let script = dir.join("render.sh");
    std::fs::write(&script, format!("#!/bin/sh\necho \"$1 $2\" >> '{}'\n", log.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = format!(
        "{AUTO}preview_protocol = \"kitty\"\npreview_theme = \"auto\"\n\n[preview]\npng = \"{} %b %m %f\"\n",
        script.display()
    );
    let s = PtySession::spawn_with_config(&[dir.to_str().unwrap()], Some(&config));
    let rendered = |want: &str| {
        let deadline = std::time::Instant::now() + WAIT;
        while std::time::Instant::now() < deadline {
            s.read_some(Duration::from_millis(50));
            if std::fs::read_to_string(&log).unwrap_or_default().lines().any(|l| l == want) {
                return true;
            }
        }
        false
    };

    s.read_until(QUERY, WAIT).expect("no startup query");
    s.write(LIGHT_REPLY);
    assert!(rendered("fbf1c7 auto"), "first render didn't get the light background");

    s.write(b"\x1b[?997;1n");
    s.read_until(QUERY, WAIT).expect("no query after the scheme notification");
    s.write(DARK_REPLY);
    assert!(rendered("282828 auto"), "no re-render against the dark background");
}
