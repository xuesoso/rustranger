//! Zero-dependency performance benchmarks for rustranger.
//!
//! Run with `cargo bench` (or `cargo bench --bench bench`). Uses a custom
//! std-only timing harness (`harness = false` in Cargo.toml) so we add no
//! benchmarking dependencies. Each benchmark times only the operation under
//! test; setup (e.g. cloning the input) happens outside the timed region.
//!
//! Reports median and min over N iterations (after warmup). Compare runs by
//! eye, or capture the output to track against a recorded baseline.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rustranger::config::Settings;
use rustranger::fs::fsobject::Entry;
use rustranger::fs::sort::{sort_entries, SortKey, SortOptions};
use rustranger::fs::Dir;
use rustranger::{preview, util};

/// Number of directory entries used for the load/sort/filter benchmarks.
const N: usize = 20_000;

struct Stats {
    median_us: f64,
    min_us: f64,
    iters: usize,
}

/// Run `op` (which returns the duration of its own timed region) `iters` times
/// after a warmup, and summarize. Letting `op` return the duration means
/// per-iteration setup can be excluded from the measurement.
fn measure(iters: usize, mut op: impl FnMut() -> Duration) -> Stats {
    for _ in 0..(iters / 5).max(2) {
        black_box(op());
    }
    let mut samples: Vec<f64> = Vec::with_capacity(iters);
    for _ in 0..iters {
        samples.push(op().as_secs_f64() * 1e6);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Stats {
        median_us: samples[samples.len() / 2],
        min_us: samples[0],
        iters,
    }
}

fn report(name: &str, s: Stats) {
    println!(
        "{:<26} median {:>11.1} us   min {:>11.1} us   (n={})",
        name, s.median_us, s.min_us, s.iters
    );
}

/// Create (once, then reuse) a directory of `n` files with varied names that
/// exercise case-insensitive and numeric sorting.
fn ensure_dir(n: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustranger_bench_{}", n));
    let ok = std::fs::read_dir(&dir).map(|rd| rd.count() == n).unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..n {
            let group = match i % 3 {
                0 => "Alpha",
                1 => "beta",
                _ => "Gamma",
            };
            // Descending numeric component so on-disk order is not pre-sorted.
            let name = format!("File_{:06}_{}.txt", n - i, group);
            std::fs::File::create(dir.join(name)).unwrap();
        }
    }
    dir
}

/// Deterministically scramble entries (order by a hash of the name) so sort
/// benchmarks measure a realistic unsorted input, identically across runs.
fn scrambled(entries: &[Entry]) -> Vec<Entry> {
    let mut v = entries.to_vec();
    v.sort_by_key(|e| {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in e.name.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    });
    v
}

fn main() {
    let settings = Settings::default();
    println!("# rustranger benchmarks (N = {} entries)", N);

    // --- directory load (readdir + lstat per entry; syscall-bound) ---
    let dir_path = ensure_dir(N);
    report(
        "dir_load",
        measure(15, || {
            let mut d = Dir::new(dir_path.clone());
            let t = Instant::now();
            d.load(&settings);
            let e = t.elapsed();
            black_box(d.files_all.len());
            e
        }),
    );

    // Load once for the CPU-bound benchmarks.
    let mut base = Dir::new(dir_path.clone());
    base.load(&settings);
    let input = scrambled(&base.files_all);

    // --- sorting (each key sorts a fresh unsorted clone) ---
    for (label, key) in [
        ("sort_natural", SortKey::Natural),
        ("sort_basename", SortKey::Basename),
        ("sort_size", SortKey::Size),
        ("sort_type", SortKey::Type),
        ("sort_extension", SortKey::Extension),
    ] {
        let opts = SortOptions {
            key,
            reverse: false,
            directories_first: true,
            case_insensitive: true,
        };
        report(
            label,
            measure(40, || {
                let mut v = input.clone();
                let t = Instant::now();
                sort_entries(&mut v, &opts);
                let e = t.elapsed();
                black_box(v.first().map(|x| x.size));
                e
            }),
        );
    }

    // --- filtering (case-insensitive substring over every entry) ---
    {
        let mut d = Dir::new(dir_path.clone());
        d.load(&settings);
        d.temporary_filter = Some("alpha".to_string());
        report(
            "filter_substring",
            measure(60, || {
                let t = Instant::now();
                d.refilter(&settings);
                let e = t.elapsed();
                black_box(d.files.len());
                e
            }),
        );
    }

    // --- preview parsing (read head, sniff, sanitize) ---
    {
        let txt = dir_path.join("_bench_text.txt");
        let mut body = String::new();
        for i in 0..1500 {
            body.push_str(&format!("line {:04}: the quick brown fox jumps over\n", i));
        }
        std::fs::write(&txt, &body).unwrap();
        let sz = body.len() as u64;
        report(
            "preview_text",
            measure(60, || {
                let t = Instant::now();
                let p = preview::load(&txt, sz);
                let e = t.elapsed();
                black_box(matches!(p, preview::Preview::Text(_)));
                e
            }),
        );
        let _ = std::fs::remove_file(&txt);
    }

    // --- text/layout helpers (proxy for per-cell render cost) ---
    {
        let names: Vec<String> = base.files_all.iter().map(|e| e.name.clone()).collect();
        report(
            "truncate_names",
            measure(60, || {
                let t = Instant::now();
                let mut acc = 0usize;
                for n in &names {
                    acc = acc.wrapping_add(util::truncate(n, 30).len());
                }
                let e = t.elapsed();
                black_box(acc);
                e
            }),
        );
        report(
            "display_width_names",
            measure(60, || {
                let t = Instant::now();
                let mut acc = 0usize;
                for n in &names {
                    acc = acc.wrapping_add(util::display_width(n));
                }
                let e = t.elapsed();
                black_box(acc);
                e
            }),
        );
    }

    // --- frame render + diff flush (the per-keystroke hot path) ---
    {
        use rustranger::app::App;
        use rustranger::screen::{self, Buffer, Style};
        use rustranger::ui;
        use crossterm::style::Color;

        let mut app = App::new(dir_path.clone(), Settings::default());
        app.prepare_view();
        let (cols, rows) = (120usize, 40usize);
        let default = Style::new(Color::Reset, Color::Reset);

        // Build the frame buffer.
        let mut cur = Buffer::new(cols, rows, default);
        report(
            "render_build",
            measure(200, || {
                let t = Instant::now();
                ui::render(&mut cur, &app);
                let e = t.elapsed();
                black_box(cur.cols);
                e
            }),
        );

        // Full (first-paint) flush: every cell emitted.
        let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
        report(
            "flush_full",
            measure(200, || {
                out.clear();
                let t = Instant::now();
                screen::flush(&mut out, None, &cur).unwrap();
                let e = t.elapsed();
                black_box(out.len());
                e
            }),
        );
        let full_bytes = out.len();

        // Incremental flush: move the cursor one row, diff against the prev frame.
        let prev = cur.clone();
        app.move_cursor(1);
        app.prepare_view();
        ui::render(&mut cur, &app);
        report(
            "flush_move_1row",
            measure(200, || {
                out.clear();
                let t = Instant::now();
                screen::flush(&mut out, Some(&prev), &cur).unwrap();
                let e = t.elapsed();
                black_box(out.len());
                e
            }),
        );
        let move_bytes = out.len();

        println!(
            "{:<26} full {} bytes  ·  1-row move {} bytes  ({}x{})",
            "frame.size", full_bytes, move_bytes, cols, rows
        );
    }
}
