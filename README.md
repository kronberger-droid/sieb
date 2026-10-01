# sieb

A dmenu-first picker for Wayland. Lines go in on stdin, the pick comes out
on stdout. Everything else, like an app launcher or a power menu, is a
script rather than a built-in mode.

![sieb as an app launcher with buttons for four modes](docs/screenshots/launcher.png)

Above is an app launcher with four modes, apps, commands, files and
windows, each a nu script from [`examples/`](examples).

sieb draws its own panel (sctk, tiny-skia, cosmic-text) and streams stdin
into a [nucleo](https://github.com/helix-editor/nucleo) matcher while the
window comes up. It needs a compositor with `wlr-layer-shell`, which
rules out GNOME. It is written against
[niri](https://github.com/YaLTeR/niri) and also runs on sway.

## Install

With nix:

```nu
nix build github:kronberger-droid/sieb
./result/bin/sieb --help
```

From source, inside `nix develop` or anywhere `pkg-config` finds
`xkbcommon` and `fontconfig`:

```nu
cargo build --release
```

## Use

### dmenu

```nu
ls | get name | to text | sieb --prompt open
```

Enter prints the highlighted row. When nothing matches, Enter prints the
query as typed, and Shift+Enter always does. sieb exits 0 on a pick and 1
on cancel, like dmenu. `--index` prints the row's position in the input
instead, and `-1` for typed text.

Matching is fuzzy and smart case: an uppercase letter in the query makes
the match case-sensitive. `-i` turns that off.

`--filter QUERY` prints every match in rank order without opening a
window, which is handy in tests and pipelines.

### JSON

`--json` reads one JSON value per line, or a single array, which is what
nu's `to json` writes for a table:

```nu
ls | to json -r | sieb --text name | from json
```

- An object with `text` is a row. It may also carry `info`, `meta` (extra
  search terms that are matched but not shown) and `selectable: false`.
- `--text FIELD` shows `FIELD` instead of `text`, so records that were not
  written for sieb need no renaming. It implies `--json` and does not work
  with `--script`.
- A record without `FIELD` falls back to `text`. One with neither shows as
  its whole JSON.
- A plain string or number is a row of just that.
- An object made only of menu option keys (`prompt`, `message`,
  `no-custom`, ...) sets those options instead.

The pick prints the original value back unchanged, unknown fields
included. Typed text prints as `{"text": ...}`, or under `FIELD` with
`--text`.

### Scripts

`--script SCRIPT` runs rofi's script protocol (`rofi-script(5)`), so rofi
scripts run unchanged:

1. sieb runs the script with no argument. What it prints is the first menu.
2. On a pick, sieb runs it again with the picked row as its argument. What
   that call prints replaces the menu.
3. A call that prints nothing ends sieb.

The script sees why it was called in `ROFI_RETV` (0 initial, 1 a row, 2
typed text), the picked row's `info` in `ROFI_INFO`, and the menu's `data`
in `ROFI_DATA`. Each also exists as `SIEB_*`, and `SIEB_QUERY` holds the
query.

Rows and menu options use rofi's in-band syntax:

```
\0prompt\x1fpower           menu option
Shutdown\0info\x1fpoweroff  row with options
```

Supported row options are `info`, `meta` and `nonselectable`. Supported
menu options are `prompt`, `message`, `data`, `no-custom`, `keep-filter`,
`new-selection` and `markup-rows`. A script whose first line is
`{"sieb": 1}` writes the rest as JSON rows instead.

With `markup-rows` on, rows are Pango markup, the subset rofi scripts
use: `<b>`, `<i>`, `<small>`, `<big>`, and `<span>` with `weight`,
`style`, `size`, `foreground` and `alpha`, plus `&amp;`-style entities.
Rows match on their text without tags. Markup that does not parse shows
as written.

Calls after a pick get an `XDG_ACTIVATION_TOKEN` (and the same value as
`DESKTOP_STARTUP_ID`), so an app the script launches may take focus.

A script that starts a long-running program has to detach its output,
since sieb waits for the script's stdout to close.

### Modes

Given more than once, `--script` makes each script a mode, like rofi's
`-modi`. Each takes an optional label, `LABEL:PATH`. Buttons under the
list show the modes by label, or by file name without one. A mode's label
is its prompt unless the script sets one.

Ctrl+Tab and Ctrl+Shift+Tab, Shift+Left and Shift+Right, or a click on a
button switch modes. The new mode starts with its initial call, and the
query carries over.

### Examples

[`examples/`](examples) rebuilds a rofi setup in nu scripts: a launcher
with four modes and a power menu. Scripts that only work under one
compositor live in a folder named after it.

- [`launcher/`](examples/launcher) works under any compositor.
  - `drun.nu` launches apps from XDG desktop entries, the most used
    first. Typed text that matches no app runs as a command.
  - `run.nu` runs a program from `PATH`, or the typed command line.
  - `files.nu` browses directories and opens files with `xdg-open`.
  - `launcher.toml` is the theme.
- [`power/`](examples/power) locks, suspends, logs out, hibernates,
  reboots or shuts down, the last five after a confirmation. Logout knows
  niri and sway; the rest works anywhere. `power.toml` is the theme.
- [`niri/`](examples/niri) needs niri.
  - `window.nu` focuses a window, as a fourth launcher mode.
- [`stream/`](examples/stream) pipes large, slow input into plain sieb,
  which is usable from the first line while the counter climbs. Run
  them from a terminal.
  - `find.nu [DIR]` lists every file under `DIR` (default `~`) with
    `fd` and opens the pick with `xdg-open`.
  - `grep.nu [DIR]` lists every line of a project with `rg` and opens
    the pick in helix at that line. Nushell's repository is 440k lines.
  - Both take `--config` for a theme and `--print` to print the pick
    instead of opening it.
- [`config.toml`](examples/config.toml) lists every config key with its
  default.

The four launcher modes in one window, labelled with Nerd Font glyphs:

```nu
(sieb --config examples/launcher/launcher.toml
    --script $"\u{f002}:examples/launcher/drun.nu"
    --script $"\u{f121}:examples/launcher/run.nu"
    --script $"\u{f07c}:examples/launcher/files.nu"
    --script $"\u{f2d0}:examples/niri/window.nu")
sieb --config examples/power/power.toml --script examples/power/power.nu
```

## Keys

| key | action |
|---|---|
| Enter | pick the highlighted row |
| Shift+Enter | pick the typed text |
| Esc, Ctrl+C, Ctrl+G | cancel |
| Up, Down, Tab, Shift+Tab | move |
| Ctrl+P, Ctrl+K / Ctrl+N, Ctrl+J | move up / down |
| Page Up, Page Down | move a page |
| Ctrl+W, Ctrl+Backspace | delete a word |
| Ctrl+U | clear the query |
| Ctrl+Tab, Shift+Right / Ctrl+Shift+Tab, Shift+Left | next / previous mode |

A click picks a row or a mode, a click outside the panel cancels, and the
wheel scrolls.

## Config

sieb reads `$XDG_CONFIG_HOME/sieb/config.toml`, or the file given with
`--config`. Every key is optional and also exists as a flag, which wins
over the file: `lines` is `--lines`, `colors.border` is `--color-border`.

```toml
font = "monospace"
lines = 12

[colors]
accent = "#89b4fa"
```

Beyond font, size and colors, a theme sets the spacing and padding of
each part, rows as rounded boxes, a placeholder, a badge pill left of the
prompt, a scrollbar, and whether the counter and separator show. Colors
left unset follow a related one, so `match` follows `accent` and
`selected-text` follows `text`. See
[`examples/config.toml`](examples/config.toml) for all keys, and the two
themes in `examples/launcher/` and `examples/power/` for full ones.

## License

[MIT](LICENSE)
