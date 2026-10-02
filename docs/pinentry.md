# Pinentry

`sieb-pinentry` asks for passwords in a sieb panel. It speaks the
pinentry protocol, so it works wherever a `pinentry-*` program does:

```json
{ "pinentry": "/path/to/sieb-pinentry" }
```

in rbw's `config.json`, or `pinentry-program /path/to/sieb-pinentry` in
`gpg-agent.conf`. The flags those clients pass (`--timeout`, `--ttyname`,
`--display`, ...) are accepted and ignored.

It is tested against rbw. gpg-agent's password prompts work the same
way, but its `CONFIRM` and `MESSAGE` dialogs are not implemented and
answer as unknown commands.

The panel shows the client's prompt and one line of text: the error from
the last attempt if there is one, else the description. Typing shows as
dots. Enter submits, Escape, Ctrl+C, Ctrl+G or a click outside cancels,
and Backspace, Ctrl+U, Ctrl+W and Ctrl+Backspace edit (the last three
clear it all, since words mean nothing in hidden text).

## Theme

`$XDG_CONFIG_HOME/sieb/pinentry.toml` themes it, with the keys of
[`config.toml`](theming.md). Without that file it uses `config.toml`,
minus the placeholder. `lines` and `counter` are ignored: there is no list.

## What it protects

- The password lives in one buffer of fixed size that is never
  reallocated and is zeroed on every edit and on exit. It is locked out
  of swap where the system allows (`RLIMIT_MEMLOCK`); when it does not,
  sieb-pinentry says so on stderr and asks anyway.
- It is never drawn: the panel shapes a dot per character, so the text
  stays out of the glyph cache and the frame.
- The process is not dumpable, so it leaves no core file and same-user
  debuggers cannot attach.
- The reply goes to stdout unbuffered, from a buffer that is wiped after.

Two copies are outside its reach: each key press arrives as a short string
from the keyboard library, and the client holds the password once it has
it. The panel takes the keyboard exclusively while it is open, so other
Wayland clients do not see the typing.
