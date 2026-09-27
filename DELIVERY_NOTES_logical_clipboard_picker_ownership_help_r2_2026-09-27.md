# Tonepoet logical clipboard picker ownership + help correction — r2

Date: 2026-09-27

Base delivery: `tonepoet_logical_clipboard_structured_metadata_paste_delivery_2026-09-27.tar.gz`

## Scope

This correction is intentionally limited to the two verified defects reported after the logical-clipboard delivery. It does not reopen or redesign the logical clipboard broker, structured metadata parser, filesystem Cut provenance, Linux host-clipboard discovery, CUE behavior, ReplayGain, Analysis, decoding, or other unrelated work.

## Corrections

### 1. Reusable picker async host-read ownership

`HostClipboardPasteTarget::FilePickerOverlay` and `HostClipboardPasteTarget::MetadataFilePicker` now carry `interaction_generation` in addition to their existing session identities.

Global-picker and metadata-embedded picker host-read requests capture `app.host_clipboard_interaction_generation` at dispatch. `event_loop::handle_message` rejects a completion when that generation no longer matches the current interaction generation, using the established status:

`Clipboard result ignored because focus changed while it was being read`

This prevents a host read requested while Address owns focus from landing in Search, or another picker text target, after an intervening key or mouse interaction. The existing picker/session identities remain in force; no new picker-specific counter, lock, timer, retry loop, or clipboard authority was added.

Focused regression coverage was added for:

- global picker: Address request, intervening focus change to Search, stale completion ignored with no picker-state mutation;
- metadata-embedded picker: the same stale-read ownership sequence;
- positive control: unchanged interaction generation still applies the host value to Address normally.

### 2. `:help clipboard` semantics

The built-in clipboard help now describes the implemented logical-clipboard contract instead of the superseded host-only model. It states that:

- Tonepoet exposes one logical clipboard;
- Tonepoet Copy/Cut retains canonical text for process lifetime before host publication;
- a readable host clipboard is authoritative for command-driven paste;
- retained Tonepoet state is used only when no readable host transport exists;
- failure of an otherwise usable host-read backend is reported rather than masked by retained state;
- terminal-supplied `Event::Paste(text)` / bracketed paste is directly authoritative;
- filesystem Cut state remains reusable only while bound to the corresponding logical clipboard identity/path projection;
- host publication failure does not invalidate a Tonepoet copy that was retained successfully;
- existing OSC 52, tmux, byobu, helper-discovery, and 64 KiB guidance remains present.

The existing clipboard-help test now asserts the retained-fallback and terminal-paste language and rejects the obsolete “each new paste first consults the host clipboard” claim.

## Validation

Available checks completed in this environment:

- targeted source-contract assertions for both picker target variants, dispatch capture, stale-read guards, focused regression test presence, and corrected help semantics: PASS;
- `git diff --no-index --check` against the prior delivery: PASS;
- exact changed-file comparison against the prior delivery: PASS;
- archive gzip integrity and clean extraction verification: performed during packaging.

Environment limitation: `cargo`, `rustc`, `rustfmt`, `nix`, and `flox` are unavailable in this sandbox. Rust compilation, focused Rust tests, rustfmt/Clippy, and the normal workspace/Flox gates therefore could not be executed here. The next environment with the repository toolchain should run the existing focused tests and normal workspace gate before final handoff.

## Exact changed files relative to the base delivery

See `CHANGED_FILES_logical_clipboard_picker_ownership_help_r2_2026-09-27.txt`.
