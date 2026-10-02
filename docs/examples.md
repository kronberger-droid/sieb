# Examples

[`examples/`](../examples) rebuilds a rofi setup in nu scripts, plus two
pipelines that show off streaming. Scripts that only work under one
compositor live in a folder named after it.

## Launcher

The screenshot in the README: four modes in one window, labelled with
Nerd Font glyphs.

```nu
(sieb --config examples/launcher/launcher.toml
    --script $"\u{f002}:examples/launcher/drun.nu"
    --script $"\u{f121}:examples/launcher/run.nu"
    --script $"\u{f07c}:examples/launcher/files.nu"
    --script $"\u{f2d0}:examples/niri/window.nu")
```

- `launcher/drun.nu` launches apps from XDG desktop entries, the most
  used first. Typed text that matches no app runs as a command.
- `launcher/run.nu` runs a program from `PATH`, or the typed command line.
- `launcher/files.nu` browses directories and opens files with
  `xdg-open`.
- `niri/window.nu` focuses a window. It needs niri.

## Power menu

```nu
sieb --config examples/power/power.toml --script examples/power/power.nu
```

Locks, suspends, logs out, hibernates, reboots or shuts down, the last
five after a confirmation. Logout knows niri and sway; the rest works
anywhere.

## Passwords

```nu
rbw unlock; sieb --script examples/rbw/rbw.nu
```

`rbw/rbw.nu` lists the Bitwarden vault through
[rbw](https://github.com/doy/rbw). Enter types the pick's username, Tab
and password in one go, and copies its TOTP code if it has one, as
rofi-rbw does. Ctrl+Enter offers the rest: either field typed or copied
alone, or the TOTP code. Copies are marked sensitive
and cleared after 30 seconds. The passwords go from rbw straight into
`wtype` or `wl-copy`, never through the script. Needs rbw, wtype and
wl-clipboard; with [`sieb-pinentry`](pinentry.md) as rbw's pinentry, the
unlock is a sieb panel too.

## Streams

Plain dmenu use with large, slow input. Run them from a terminal.

- `stream/find.nu [DIR]` lists every file under `DIR` (default `~`) with
  `fd` and opens the pick with `xdg-open`.
- `stream/grep.nu [DIR]` lists every line of a project with `rg` and opens
  the pick in helix at that line. Nushell's repository is 440k lines.

Both take `--config` for a theme and `--print` to print the pick instead
of opening it.
