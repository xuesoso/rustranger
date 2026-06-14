# rustranger

A minimal, standalone **Rust** rewrite of the [ranger](https://ranger.github.io/)
file-manager TUI. It keeps full file-browsing parity while dropping ranger's
Python runtime, the curses dependency, the ~40 external preview/VCS binaries, and
the Python/bash configuration surface.

**Dependencies:** only [`crossterm`](https://crates.io/crates/crossterm) (terminal
I/O, pure Rust) and [`libc`](https://crates.io/crates/libc) (user/group name
lookup). Run `cargo tree` to confirm the tree stays minimal.

## Build & run

```sh
cargo build --release
./target/release/rustranger [PATH]
```

As a file picker (writes the chosen file's path and exits):

```sh
rustranger --choosefile /tmp/choice    # then pick a file and press Enter
```

Write a starter config file (only if one doesn't already exist):

```sh
rustranger gen-config
```

## Features

Directory browsing with the miller-columns layout (parent | current | preview),
native text-file preview, sorting, filtering, search, marking/visual selection,
copy/cut/paste (background, with progress + collision-safe naming),
delete/rename/mkdir/touch/chmod, symlink/hardlink paste, tabs, persistent
bookmarks, in-session tags, navigation history, a configurable size/date column,
light & dark color themes, a `:` command console, key-chain hint menus, a
scrollable `?` help, and opening files in external programs.

**Dropped** vs. ranger (by design): image previews, VCS status decoration, the
Python plugin system, `scope.sh` and external preview helpers, multipane view.

## Keybindings

| Key | Action | Key | Action |
|-----|--------|-----|--------|
| `j`/`k` `↓`/`↑` | move (prefix with a count, e.g. `3j`) | `space` | mark + move down |
| `h`/`l` `←`/`→` | parent / enter or open | `v` | visual select |
| `gg` / `G` | top / bottom | `V` | toggle all marks |
| `Ctrl-d`/`Ctrl-u` | half-page down/up | `uv` | clear marks |
| `J`/`K` | scroll preview | `yy` | copy |
| `z` | toggle hidden | `dd` | cut |
| `gh` / `g/` | home / root dir | `pp` | paste |
| `H` / `L` | history back / forward | `pl` / `pL` | paste symlink (rel/abs) |
| `Tab` / `BackTab` | next / prev tab | `ph` | paste hardlink |
| `gn` | new tab | `D` | delete (with confirm) |
| `Alt-1`..`9` | go to tab N | `cw` | rename |
| `t` | toggle tag | `/` then `n`/`N` | search / next / prev |
| `m`*x* / `` ` ``*x* | set / go to bookmark *x* | `um`*x* | delete bookmark *x* |
| `o` | sort menu (see below) | `:clearbookmarks` | clear all bookmarks |
| `:` | command console | `q` / `Q` | close tab / quit all |
| `?` | help (scrollable key list) | | |

Pressing a multi-key prefix (`o`, `g`, `d`, `y`, `p`, `u`, `c`, `m`, `` ` ``) pops
up a hint menu listing the keys that complete it — like ranger's keychain hints.
Press `?` for a full scrollable list of every binding.

**Sort** — press `o` to open the sort menu, then a key. **Lowercase sorts
ascending, the SHIFTed uppercase key sorts descending** (ranger's convention):
`os`/`oS` size, `on`/`oN` natural, `ob`/`oB` basename, `om`/`oM` mtime,
`oc`/`oC` ctime, `oa`/`oA` atime, `ot`/`oT` type, `oe`/`oE` extension;
`oz` random, `or` toggle reverse, `of` toggle dirs-first.

## Console commands (`:`)

`cd`, `mkdir`, `touch`, `rename`, `delete`, `chmod <octal>`, `filter <text>`,
`search <text>`, `set <option> <value>`, `shell <cmd>`, `open_with <program>`,
`pager`, `delbookmark <key>`, `clearbookmarks`, `q` / `quit`, `qa` / `quitall`.

## Configuration

Optional, at `$XDG_CONFIG_HOME/rustranger/config.toml` (or
`~/.config/rustranger/config.toml`). All keys are optional and override the
built-in defaults. Generate a fully-annotated starter file (never overwrites an
existing one) with:

```sh
rustranger gen-config
```

```toml
[settings]
show_hidden = false
sort = "natural"            # natural|basename|size|mtime|ctime|atime|type|extension|random
sort_reverse = false
sort_directories_first = true
sort_case_insensitive = true
column_ratios = "1,3,4"
preview_files = true
draw_borders = true
confirm_on_delete = true
wrap_scroll = false
show_date = true            # show a date column next to the size (current column)
time_type = "modified"      # modified|created|changed|accessed
time_format = "date"        # date (YYYY/MM/DD) | datetime (YYYY/MM/DD/HH/MM)
size_format = "human"       # human (1.5k) | binary (1.5K, 1024-based) | bytes (1536)
theme = "default"           # see "Color themes" below
```

`time_type` follows the Unix timestamps: `modified` (mtime), `changed` (ctime —
the Linux inode change-time convention), `accessed` (atime), and `created`
(birthtime where the platform/FS supports it — native on macOS, via statx on
recent Linux; falls back to mtime otherwise).

### Color themes

`theme` selects a built-in palette (these paint the background, so light themes
are genuinely light):

| Dark | Light |
|------|-------|
| `gruvbox-dark` · `solarized-dark` · `nord` · `dracula` | `gruvbox-light` · `solarized-light` · `one-light` · `ayu-light` |

`default` keeps the terminal's own background and uses the classic ANSI colors.
Individual roles can be customized on top of the chosen theme with a `[theme]`
section (values are `#rrggbb`, a basic color name, an ANSI index `0`–`255`, or
`reset`):

```toml
[settings]
theme = "gruvbox-dark"

[theme]
dir = "#ff8800"     # override just the directory color
accent = "cyan"
bg = "#11131a"      # tweak the background
```

Roles: `bg`, `fg`, `border`, `title`, `accent`, `dir`, `link`, `exec`, `special`,
`device`, `broken`, `warning`, `error`, `info`, `progress`.

### Command-line overrides

Any setting can be overridden for a single run; these win over `config.toml`:

```sh
rustranger --theme nord --sort size --reverse --time-format datetime
rustranger --set sort=mtime --set theme=ayu-light   # generic form (any setting)
rustranger --no-date                                # hide the date column
```

The active theme is also switchable at runtime with `:set theme <name>`. Run
`rustranger --help` for the full flag list.

Bookmarks persist under `$XDG_DATA_HOME/rustranger/bookmarks`. Tags (`t`) are
in-session only and are not written to disk. File opening uses `$VISUAL`/`$EDITOR`
for text and `xdg-open` (`open` on macOS) otherwise; `:shell` uses `$SHELL`.

## Development

The crate is split into a library (`src/lib.rs`) and a thin binary
(`src/main.rs`), so tests and benchmarks can drive the internals directly.

```sh
cargo test                 # unit tests
cargo clippy --all-targets # lints (kept at zero warnings)
cargo bench --bench bench  # zero-dependency micro-benchmarks for the hot paths
```

Rendering uses a cell back-buffer that diffs successive frames and writes only
the cells that changed (ncurses-style), so a cursor move repaints a couple of
rows rather than the whole screen — which keeps it smooth over SSH and inside
tmux. Benchmark baselines and the optimization log live in
[`PERFORMANCE.md`](PERFORMANCE.md).
