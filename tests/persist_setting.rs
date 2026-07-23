//! End-to-end check that `persist_setting` writes a runtime toggle to the real
//! config file and that `Settings::load` reads it back. Runs in its own process
//! (an integration test), so pointing `XDG_CONFIG_HOME` at a temp dir here can't
//! disturb the in-process unit tests.

use rustranger::config::{self, Settings};

#[test]
fn preview_images_toggle_persists_across_load() {
    // Isolate the config location to a unique temp dir for this test process.
    let dir = std::env::temp_dir().join(format!("rustranger-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("XDG_CONFIG_HOME", &dir);

    // Default is on.
    assert!(Settings::load().preview_images);

    // Toggle off and persist; a fresh load must observe it (file was created).
    config::persist_setting("preview_images", "false").expect("persist off");
    assert!(!Settings::load().preview_images, "off persisted");
    let cfg = dir.join("rustranger").join("config.toml");
    assert!(cfg.exists(), "config file seeded on first write");

    // Toggle back on and persist; load observes the new value.
    config::persist_setting("preview_images", "true").expect("persist on");
    assert!(Settings::load().preview_images, "on persisted");

    // The rest of the seeded template survives the surgical rewrite.
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(text.contains("[preview]"), "other sections untouched");
    assert_eq!(text.matches("preview_images =").count(), 1, "single key line");

    let _ = std::fs::remove_dir_all(&dir);
}
