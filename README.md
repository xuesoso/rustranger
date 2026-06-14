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

## Features

Directory browsing with the miller-columns layout (parent | current | preview),
native text-file preview, sorting, filtering, search, marking/visual selection,
copy/cut/paste (background, with progress + collision-safe naming),
delete/rename/mkdir/touch/chmod, symlink/hardlink paste, tabs, persistent
bookmarks and tags, navigation history, a `:` command console, and opening files
in external programs.

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
| `gn` | new tab | `dD` | delete (with confirm) |
| `Alt-1`..`9` | go to tab N | `cw` | rename |
| `t` | toggle tag | `/` then `n`/`N` | search / next / prev |
| `m`*x* / `` ` ``*x* | set / go to bookmark *x* | `o`*x* | sort (see below) |
| `:` | command console | `q` / `Q` | close tab / quit all |

**Sort** (`o` prefix): `os` size, `on` natural, `ob` basename, `om` mtime,
`oc` ctime, `oa` atime, `ot` type, `oe` extension, `oz` random, `or` reverse,
`of` toggle dirs-first.

## Console commands (`:`)

`cd`, `mkdir`, `touch`, `rename`, `delete`, `chmod <octal>`, `filter <text>`,
`search <text>`, `set <option> <value>`, `shell <cmd>`, `open_with <program>`,
`pager`, `q` / `quit`, `qa` / `quitall`.

## Configuration

Optional, at `$XDG_CONFIG_HOME/rustranger/config.toml` (or
`~/.config/rustranger/config.toml`). All keys are optional and override the
built-in defaults:

```toml
[settings]
show_hidden = false
sort = "natural"            # natural|basename|size|mtime|ctime|atime|type|extension|random
sort_reverse = false
sort_directories_first = true
sort_case_insensitive = true
scroll_offset = 8
column_ratios = "1,3,4"
preview_files = true
draw_borders = true
confirm_on_delete = true
wrap_scroll = false
```

Bookmarks and tags persist under `$XDG_DATA_HOME/rustranger/`
(`bookmarks`, `tagged`). File opening uses `$VISUAL`/`$EDITOR` for text and
`xdg-open` (`open` on macOS) otherwise; `:shell` uses `$SHELL`.
