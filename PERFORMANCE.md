# Performance tracking

Reproducible micro-benchmarks for the hot paths, plus a log of optimization
work. Run with:

```sh
cargo bench --bench bench
```

The harness (`benches/bench.rs`) is zero-dependency (std timing, `harness =
false`). It reports **median** and **min** over N iterations after warmup;
setup (e.g. cloning sort input) is excluded from the timed region. Numbers are
machine-dependent — always compare runs on the same machine.

Hardware for the recorded numbers: Linux x86_64 (Fedora, kernel 7.x), release
build (`opt-level=3`, `lto=true`). Dataset: 20,000 directory entries.

End-to-end context (measured separately via a pty harness vs. Python ranger,
earlier): startup ~2.5 ms / 3.7 MB; load+render 50k entries ~40 ms / 13.6 MB —
already ~6–52× faster and ~10–16× lighter than ranger. This file tracks the
*internal* CPU hot paths so we can keep improving without regressing.

## Baseline (commit fc0508a5, before perf/optimize work)

| benchmark | median | min | notes |
|---|---:|---:|---|
| `dir_load` (20k) | 13452.9 µs | 12763.3 µs | readdir + lstat/entry; syscall-bound (near-irreducible) |
| `sort_natural` | 7923.2 µs | 7792.8 µs | **default sort**; `natural_cmp` per-char w/ peekable |
| `sort_basename` | 4675.3 µs | 4578.9 µs | |
| `sort_size` | 4805.2 µs | 4736.6 µs | |
| `sort_type` | 4848.9 µs | 4772.1 µs | |
| `sort_extension` | 7929.0 µs | 7834.0 µs | `Entry::extension()` re-parsed O(n log n) times |
| `filter_substring` | 346.9 µs | 343.6 µs | `filter.to_lowercase()` recomputed per entry + per-entry alloc |
| `preview_text` | 114.8 µs | 113.7 µs | read head + sniff + sanitize; fine |
| `truncate_names` | 1054.8 µs | 935.7 µs | per-cell render proxy (20k names) |
| `display_width_names` | 933.2 µs | 927.9 µs | per-cell render proxy |

## Optimization log

### 1. Filter: hoist needle + allocation-free substring match
`Dir::refilter`/`is_visible` lower-cased `filter.to_lowercase()` **per entry** and
allocated a lower-cased copy of every name. Now the needle is lower-cased once and
matched with an allocation-free ASCII-folding `ci_contains`.
- `filter_substring`: **346.9 → 277.8 µs** (~20% faster), 0 per-entry allocations.
- Guards: `temporary_filter_and_hidden_visibility`, `ci_contains_matches_case_insensitively`.

### 2. Render: no screen clear + buffered single write  ← flicker fix
**Symptom:** flicker when navigating rows quickly (reported on Ghostty inside tmux
over SSH). **Two causes, both fixed:**

1. *Mid-frame flush.* Frames were written straight to `io::stdout()` (a
   `LineWriter`); a dense miller frame has no newlines and overflows the 8 KB
   buffer, flushing mid-frame so the terminal painted half-drawn screens. **Fix:**
   `ui::draw` → `ui::render(out, app, cols, rows)` renders into a reusable
   `Vec<u8>`; the run loop writes the whole frame in one `write_all` + `flush`.

2. *Full-screen clear (the tmux flicker).* The frame began with `Clear(All)` =
   `ESC[2J`, which resets the terminal/tmux screen model to all-blank and forces a
   **physical repaint of every cell** each keystroke — under tmux+SSH that whole-
   pane repaint is the flicker. (Synchronized-update `ESC[?2026` doesn't reliably
   pass through tmux, so it can't save us here.) **Fix:** never clear. `render()`
   repaints blank background cells *in place* with literal spaces (no `ESC[2J`, no
   `ESC[K`), then draws content on top. Unchanged cells stay byte-identical frame
   to frame, so the terminal's own cell diff repaints only what actually moved.

The frame is still wrapped in `Begin`/`EndSynchronizedUpdate` as a bonus for
terminals that do honor it.
- `render_frame`: **205.7 µs** median, **11195 bytes/frame** (120×40). Bytes went
  up vs. the clear-based frame (6 KB) because we now emit the blank cells, but the
  frame goes to the *local* tmux pty (not over SSH); tmux diffs it and forwards only
  changed cells to the client.
- Guards: `render_emits_one_synchronized_frame_with_content` now also asserts the
  frame contains **no `ESC[2J` / `ESC[2K` / `ESC[K`** (locks the no-clear contract).

### 3. Render: coalesce input bursts → one frame per burst  ← flicker fix (fast nav)
**Symptom (follow-up):** flicker still bad when navigating quickly, worse in a
fullscreen tmux pane over SSH. **Cause:** the loop rendered one full frame per
keypress; held-key repeat produces frames faster than tmux→client can repaint over
SSH, so frames back up and the display thrashes. **Fix:** after handling the first
event, drain every event already queued (`event::poll(ZERO)`) before the next
redraw. When `write()` back-pressures behind a slow redraw, keystrokes queue and a
whole burst collapses into a single render — self-pacing renders to the display's
real rate. Verified: a 12-key burst now renders **1 frame** (was ~12), cursor still
advances 12. Bonus: intermediate previews are skipped during a burst.

### 4. Render: cell back-buffer + diff (emit only changed cells)  ← flicker fix
**Symptom (2nd follow-up):** still bad on fast scroll in a fullscreen tmux pane.
**Cause:** even with no clear, the renderer rewrote *every* cell each frame
(~11 KB) — far more than ncurses-based tools — so we flooded tmux→client with full
panes ~30×/s over SSH. **Fix (the real one):** a cell back-buffer (`src/screen.rs`):
`ui::render` draws into an in-memory grid; `screen::flush` emits escape sequences
only for the cells that **differ** from the previously displayed frame (double-
buffered). The whole screen is still "repainted" logically every frame, but the
wire output is just the delta. A screen clear is emitted exactly once, on first
paint / resize. `DisableLineWrap` avoids autowrap when the last cell is written.
- `render_build` 218 µs (fill grid), `flush_full` 55 µs, `flush_move_1row` 24 µs.
- **bytes/frame: full paint 8.6 KB; a 1-row cursor move = 189 µs-bench / ~380 B
  over a real pty — ~30–46× less than the previous full-screen frame.** This is the
  ncurses/ratatui model and brings us to ranger-level wire efficiency.
- Guards: `render_then_flush_emits_clean_frame_with_content` (clean frame, content,
  no clear sequences) and `diff_flush_emits_only_changed_cells` (a one-cell change
  flushes < 64 bytes).
- Kept from earlier: input coalescing (#3), synchronized-update wrap, single write.
  The no-clear blank-fill (#2) is superseded by the diff (which inherently never
  rewrites unchanged cells).

### Deferred
- `sort_natural` 7.9 ms / `sort_extension` 7.9 ms (CPU comparators): a byte-wise
  `natural_cmp` and a precomputed extension key would help, but sorting is not the
  user-facing bottleneck (load is syscall-bound; sort runs once per cd/re-sort).
