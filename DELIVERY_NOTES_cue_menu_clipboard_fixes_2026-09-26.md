# Delivery notes: CUE menu + unified filesystem clipboard fixes

Date: 2026-09-26
Base bundle: `tonepoet_snapshot_main_6cd5b4f_2026-09-26.tar.gz`
Base snapshot: `main` at `6cd5b4f` (per bundle name)

## Scope

This delivery is intentionally limited to the two reported defect families:

1. Browse CUE menu cache freshness / async ownership / visible menu-geometry changes.
2. Filesystem Cut/Copy/Paste using retained in-process state as a second user-visible clipboard instead of treating the terminal/host clipboard as authoritative for every new Paste.

No unrelated architecture or hardening was added.

## CUE corrections

- Folder-classification requests now carry a monotonic request ID in addition to scan generation and exact-vs-ordinary intent. Completion is accepted only if all three still own the path's pending slot. An older worker can no longer consume a newer worker's pending marker or publish over it.
- Exact CUE results now carry a fingerprint of the concrete CUE/audio member files they inspected. The directory identity still guards add/remove/rename changes; member identities additionally catch in-place edits whose containing-directory mtime does not change.
- A cached exact CUE result is reused only while both the folder identity and the member fingerprint remain current. If the member fingerprint is missing/stale, cached CUE facts are demoted to `Unknown` before menu construction and an exact request is scheduled. A stale positive therefore cannot create the first-frame `Advanced CUE Options` false positive described in the defect report.
- Exact-worker completion revalidates its member fingerprint before publishing CUE authority. If a relevant file changed while the worker was running, the useful folder classification is retained but its CUE facts remain `Unknown`.
- Ordinary folder-classification work cannot erase a newer exact result through the old path-only pending race.
- Unresolved CUE facts no longer create provisional disabled `Repair malformed CUE (create copy)` or embedded-CUE rows that disappear on `Unknown -> Absent`.
- While a Browse context menu is open, a background completion may update enabled state only when the menu has identical physical geometry. If the new classification would add/remove rows or submenus, the currently open menu remains unchanged and the corrected shape appears on the next open.

The result deliberately favors a stable menu over changing its shape underneath the pointer. On a never-before-classified CUE-positive folder, CUE-specific rows may therefore first appear on the next menu open after the asynchronous exact probe completes.

## Filesystem clipboard corrections

- The host clipboard is now authoritative for every new filesystem Paste in Browse and the reusable file picker.
- Copy/Cut still mirrors the newline path projection to the host clipboard and retains structured transaction/retry metadata internally.
- A new Paste reuses retained structured state only if the current host text exactly matches the retained path projection (allowing terminal-reader CRLF/trailing-newline normalization). This preserves Cut-vs-Copy and retry identity without retaining a second authoritative clipboard.
- If host text changed, valid path lines become a fresh Copy transaction. Invalid/nonexistent path text fails explicitly; Tonepoet does not fall back to stale retained paths.
- Bracketed `Event::Paste(text)` consumes the terminal-provided payload directly. It no longer substitutes retained Browse/file-picker clipboard state.
- Ctrl+V, Ctrl+P, and raw Ctrl+Shift+V on filesystem navigation all request/use the host clipboard. File-transfer progress overlay paste follows the same contract.
- File-picker asynchronous paste destinations are frozen at request time. A newer text-editor paste request clears an older frozen filesystem destination so a later host result cannot be misrouted.

Transport limitation retained from the existing host-clipboard layer: native host reads use `pbpaste`, `wl-paste`, `xclip`, or `xsel`. OSC 52 remains a write-only fallback here. In an SSH/tmux session with no readable host clipboard provider, Ctrl+V/Ctrl+P can report host-read unavailability; terminal-intercepted bracketed Ctrl+Shift+V remains able to provide the authoritative payload.

## Changed files relative to the supplied bundle

- `crates/tui-file-picker/src/filesystem_clipboard.rs`
- `crates/tui-file-picker/src/input.rs`
- `crates/tui-file-picker/src/lib.rs`
- `crates/tui-file-picker/src/state.rs`
- `src/tui/browse.rs`
- `src/tui/command.rs`
- `src/tui/context_menu.rs`
- `src/tui/event_loop.rs`
- `src/tui/keybindings.rs`
- `src/tui/message.rs`
- `DELIVERY_NOTES_cue_menu_clipboard_fixes_2026-09-26.md` (this file)

## Regression coverage added/updated

Coverage now exercises, among other paths:

- resolved CUE cache without an exact member fingerprint does not suppress revalidation;
- an in-place CUE edit demotes a cached positive before menu construction;
- an old classification worker cannot consume a newer pending owner;
- open-menu geometry stays fixed across positive-to-absent and unresolved-to-absent transitions;
- unresolved CUE facts do not create provisional repair/embedded rows;
- retained Cut state survives only while host text still matches it;
- changed/non-path host text cannot fall back to stale retained filesystem state;
- bracketed filesystem paste replaces stale retained state with its supplied host paths;
- asynchronous Browse host reads use the destination captured at dispatch time;
- Ctrl+V, Ctrl+P, and Ctrl+Shift+V request the authoritative host clipboard for Browse/file-picker navigation;
- a newer picker text-paste request clears an older frozen filesystem destination;
- the live file-transfer overlay no longer pastes retained transaction state directly.

## Validation performed in this runner

The execution environment contains no `cargo`, `rustc`, or `rustfmt`, and outbound network resolution is unavailable, so the Rust workspace could not be compiled, formatted, or test-run here. This delivery therefore does **not** claim that Cargo tests passed in this sandbox.

Static validation performed:

- audited every `FolderClassifyComplete` constructor/consumer after the message-shape change;
- audited user-visible filesystem paste routes for direct retained-clipboard execution;
- audited direct host-paste request-state writes in the reusable picker;
- checked the modified Rust files with a lexical delimiter-balance scan;
- ran `git diff --no-index --check` against the supplied tree; it reported no whitespace-error diagnostics (exit status is nonzero because the trees intentionally differ).

Recommended handoff gate on a Rust 1.93-capable host:

```sh
cargo fmt --check
cargo test -p tui-file-picker
cargo test --workspace
```

At minimum, the focused CUE/clipboard regression tests added in the files above should be run if the full workspace gate is impractical.
