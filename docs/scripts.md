# Scripts and modes

`--script SCRIPT` runs rofi's script protocol (`rofi-script(5)`), so rofi
scripts run unchanged:

1. sieb runs the script with no argument. What it prints is the first menu.
2. On a pick, sieb runs it again with the picked row as its argument. What
   that call prints replaces the menu.
3. A call that prints nothing ends sieb.

The script sees why it was called in `ROFI_RETV` (0 initial, 1 a row, 2
typed text, 10 either picked with Ctrl+Enter, as rofi's first custom key),
the picked row's `info` in `ROFI_INFO`, and the menu's `data`
in `ROFI_DATA`. Each also exists as `SIEB_*`, and `SIEB_QUERY` holds the
query.

A script that starts a long-running program has to detach its output,
since sieb waits for the script's stdout to close.

## Rows and options

Rows and menu options use rofi's in-band syntax:

```
\0prompt\x1fpower           menu option
Shutdown\0info\x1fpoweroff  row with options
```

Supported row options are `info`, `meta` and `nonselectable`. Supported
menu options are `prompt`, `message`, `data`, `no-custom`, `keep-filter`,
`new-selection` and `markup-rows`.

A script whose first line is `{"sieb": 1}` writes the rest as JSON rows
instead, in the same shape as [`--json`](dmenu.md#json) input.

## Markup

With `markup-rows` on, rows are Pango markup, the subset rofi scripts
use: `<b>`, `<i>`, `<small>`, `<big>`, and `<span>` with `weight`,
`style`, `size`, `foreground` and `alpha`, plus `&amp;`-style entities.
Rows match on their text without tags. Markup that does not parse shows
as written.

## Focus for launched apps

Calls after a pick get an `XDG_ACTIVATION_TOKEN` (and the same value as
`DESKTOP_STARTUP_ID`), so an app the script launches may take focus.

## Modes

Given more than once, `--script` makes each script a mode, like rofi's
`-modi`. Each takes an optional label, `LABEL:PATH`. Buttons under the
list show the modes by label, or by file name without one. A mode's label
is its prompt unless the script sets one.

```nu
(sieb --script apps:examples/launcher/drun.nu
    --script files:examples/launcher/files.nu)
```

Ctrl+Tab and Ctrl+Shift+Tab, Shift+Left and Shift+Right, or a click on a
button switch modes. The new mode starts with its initial call, and the
query carries over.
