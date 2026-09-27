# Linux clipboard transport discovery — delivery notes

Date: 2026-09-26

Base delivery: `tonepoet_cumulative_metadata_clipboard_identity_delivery_2026-09-26.tar.gz`

Base SHA-256: `36ac8cd11a891bad3f259dc403a6a34a23a9a9a293384ca93394e47da0a9db44`

## Correction

This is a narrow host-clipboard transport correction. It does not change metadata envelope semantics, CUE handling, filesystem clipboard transaction state, persistence, conversion, or source-lock code.

On Linux, `ClipboardEnvironment::detect()` now treats a usable inherited display as authoritative and takes a zero-discovery fast path in that case. When the immediate environment is incomplete, it recovers missing graphical session context from the nearest same-effective-UID process ancestors. Recovery is bounded and reads only the clipboard-relevant variables from `/proc/<pid>/environ`: `WAYLAND_DISPLAY`, `DISPLAY`, `XDG_RUNTIME_DIR`, `XAUTHORITY`, plus SSH markers used to prevent unsafe local Wayland inference in remote sessions. A recovered Wayland display is paired with that ancestor's runtime directory, and a recovered X11 display is paired with that ancestor's Xauthority when available.

If no Wayland display name is available but a local `XDG_RUNTIME_DIR` contains exactly one live `wayland-*` Unix socket, Tonepoet may infer that socket name. It does not guess among multiple Wayland sockets and does not guess a global X11 display such as `:0`.

Recovered values are passed only to the `wl-copy`/`wl-paste`/`xclip`/`xsel` child process with `Command::env`; Tonepoet does not mutate its process-global environment. This keeps clipboard reads/writes thread-safe and gives Ctrl+V/Ctrl+P access to a readable local/forwarded clipboard when the immediate Tonepoet environment lost its display variables.

`:clipboard` diagnostics now show resolved display values, their source, `XDG_RUNTIME_DIR`, `XAUTHORITY`, and whether an SSH lineage was detected.

A genuinely remote OSC-52-only connection still has no standards-safe host clipboard read transport for Ctrl+V/Ctrl+P. Tonepoet continues to use terminal-supplied bracketed paste for Ctrl+Shift+V in that case rather than introducing a private text clipboard or racing the TUI input reader with an OSC 52 query.

## Changed files relative to the base delivery

- `src/tui/host_clipboard.rs`
- `DELIVERY_NOTES_linux_clipboard_transport_discovery_2026-09-26.md`

## Focused regression coverage added

- Linux `/proc/<pid>/environ` parsing extracts only relevant display/session variables.
- Nearest same-UID ancestor fills missing display context without overriding inherited values.
- Recovered X11 `DISPLAY`/`XAUTHORITY` makes the native read path eligible.
- A unique local Wayland runtime socket can supply a missing `WAYLAND_DISPLAY`.
- Remote sessions do not infer a local Wayland socket.
- Multiple Wayland sockets are treated as ambiguous rather than guessed.
- Native clipboard helper subprocesses receive the resolved graphical environment explicitly.

## Validation and limitations

The execution environment contains no `cargo`, `rustc`, `rustfmt`, Nix, or Flox executable, so Rust formatting and workspace tests could not be executed here. Static validation includes patch/whitespace checks, changed-file accounting, delimiter/syntax-oriented inspection, and archive round-trip comparison. The Linux process-lineage recovery model was also exercised independently against `/proc/<parent>/environ` with a child whose `DISPLAY`/`XAUTHORITY` were removed, confirming that the parent retains the recoverable values.

Handoff gate:

```sh
cargo fmt --check
cargo test host_clipboard --lib
cargo test --workspace
```
