# sieb

dmenu-first Wayland picker. User-facing docs are in `README.md` and `docs/`.

- `nix flake check` is the gate before a push, the same as CI: the build with
  its tests, clippy with `-D warnings`, rustfmt, and a parse of
  `examples/**/*.nu`. A change to `flake.nix` also gets a `nix build`.
- The example scripts run on whichever nu the user brings: they keep
  `#!/usr/bin/env nu`, and nushell stays out of the package's inputs.
