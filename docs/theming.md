# Theming

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
prompt, a scrollbar, and whether the counter and separator show.

`blur = true` asks the compositor to blur what is behind the panel,
through `ext-background-effect-v1`. It shows where `colors.background`
is not opaque, and a compositor without the protocol draws the panel as
before. On niri the blur is of the wallpaper alone unless a layer rule
for the `sieb` namespace sets `background-effect { xray false }`.

Colors left unset follow a related one, so `match` follows `accent` and
`selected-text` follows `text`. A theme only names what differs.

[`examples/config.toml`](../examples/config.toml) lists every key with
its default and fallback. Two complete themes, recreating a rofi setup:

- [`launcher.toml`](../examples/launcher/launcher.toml): row boxes, a
  scrollbar and mode buttons.
- [`power.toml`](../examples/power/power.toml): badge and prompt pills
  and a message box.

Per-menu themes need nothing special: each menu runs with its own
`--config`.
