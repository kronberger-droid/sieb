# dmenu use

Lines go in on stdin, the pick comes out on stdout.

```nu
ls | get name | to text | sieb --prompt open
```

Enter prints the highlighted row. When nothing matches, Enter prints the
query as typed, and Shift+Enter always does. sieb exits 0 on a pick and 1
on cancel, like dmenu. `--index` prints the row's position in the input
instead, and `-1` for typed text.

Matching is fuzzy and smart case: an uppercase letter in the query makes
the match case-sensitive. `-i` turns that off.

Rows are matched as they arrive, so a slow producer like `fd` over a large
home is usable from its first line while the counter climbs.

`--filter QUERY` prints every match in rank order without opening a
window, which is handy in tests and pipelines.

## JSON

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
| Ctrl+Tab, Shift+Right / Ctrl+Shift+Tab, Shift+Left | next / previous [mode](scripts.md#modes) |

A click picks a row or a mode, a click outside the panel cancels, and the
wheel scrolls.
