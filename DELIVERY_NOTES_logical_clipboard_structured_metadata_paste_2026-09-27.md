# Tonepoet logical clipboard and structured metadata paste correction

Date: 2026-09-27
Base: `tonepoet_snapshot_main_3957fe4_2026-09-26.tar.gz` (`3957fe4` snapshot naming)

## Result

This delivery implements one logical clipboard authority with a process-lifetime retained fallback. A readable host clipboard remains authoritative when observable. When no readable host transport exists, command-driven paste uses the retained Tonepoet value. A terminal-supplied bracketed paste is authoritative immediately and never triggers a host reread.

The file/folder regression called out during implementation is restored: after Tonepoet `Ctrl+C` or `Ctrl+X`, navigation to another directory followed by `Ctrl+V`/`Ctrl+P` works without Wayland/X11 by using the retained logical clipboard generation. Cut semantics are preserved only while the structured filesystem transaction remains bound to that exact logical generation/canonical path projection.

## Verified defects in the base

- Command-driven paste always attempted a host clipboard read. In a remote/headless session with no readable Wayland/X11 backend, Browse/picker paste failed before the retained filesystem transaction could be used.
- Metadata structured-envelope recognition used literal LF-bearing prefixes before transport framing normalization. CRLF input could miss the versioned envelope and fall into raw-field semantics, producing the reproduced GENRE cardinality error.
- Bracketed terminal paste on structured metadata paths could be reconciled against a host reread, allowing the supplied terminal payload to be replaced.
- Filesystem transaction state and text clipboard authority were separate enough that headless fallback, external authority, and destructive Cut provenance were not encoded by one generation identity.

## Implementation

### Logical clipboard broker

`crates/tui-file-picker/src/text_input.rs` now owns a small process-lifetime logical clipboard snapshot with canonical text, generation, and provenance (`TonepoetCopy`, `HostRead`, `TerminalPaste`).

Tonepoet copy paths synchronously retain canonical text before host publication. The retain + host-queue handoff is serialized so two concurrent copy producers cannot queue an older generation after a newer generation. The retained snapshot is memory-only.

A successful host read with different text advances logical generation. An explicit terminal paste always advances generation, even for identical text, so stale destructive Cut provenance cannot survive a bracketed paste.

### Host acquisition and ordering

`src/tui/host_clipboard.rs` keeps the existing host discovery behavior and write queue. Command paste now:

1. waits for preceding Tonepoet publication work;
2. reads a native host clipboard when a readable candidate exists;
3. falls back to retained logical text only when no readable native transport exists;
4. surfaces an actual backend failure instead of silently substituting retained text;
5. discards an asynchronous read before delivery if a newer logical clipboard generation superseded the request.

External publication failure does not invalidate a successfully retained Tonepoet copy. Metadata copy still uses the write-drain barrier as ordering, without claiming OSC 52 acknowledgement.

### Filesystem Cut/Copy provenance

`FilesystemClipboard` carries process-lifetime logical generation/text identity. Those fields use `#[serde(skip)]`; they are deliberately not persisted.

Browse and the reusable picker reuse retained structured Cut/Copy state only when it is bound to the current logical generation and canonical path projection. Native-helper trailing newline framing is accepted for the bound filesystem projection without weakening path equality. Different readable host text becomes a fresh Copy transaction. Terminal paste advances generation first, so it cannot inherit an older Cut.

Residual retry transactions inherit the original logical identity so partial Cut retry semantics remain correct while the same logical clipboard generation is current.

### Terminal paste convergence

`Event::Paste(text)` now retains `text` as a new terminal-origin logical generation before application, invalidates older command-read generations, and applies that exact payload directly. It no longer rereads the host clipboard for metadata reconciliation.

An ordinary `Ctrl+Shift+V` key chord remains part of the same command-driven clipboard route as `Ctrl+V`/`Ctrl+P`.

### Structured metadata framing and fail-closed parsing

`parse_clipboard_tag_payload()` now recognizes the Tonepoet namespace before parsing, normalizes only versioned-envelope transport framing (LF, CRLF, and bare CR) to LF, then recognizes the envelope header. Structured values containing CR/LF are JSON-escaped by the serializer, so their contents are not trimmed or rewritten.

Recognizable Tonepoet envelopes fail explicitly on unsupported versions/kinds, malformed framing, malformed field blocks, or malformed `@tonepoet-mv1:` JSON. They no longer fall through to raw-row paste.

The reproduced one-field GENRE FieldSet parses as one positional element containing five ordered stored values. One source position is classified as broadcast and applies the complete five-value list to every one of eight destination positions.

## Regression coverage added/updated

Focused source tests now cover:

- LF, CRLF, and bare-CR structured-envelope framing;
- fail-closed malformed/unsupported Tonepoet envelopes;
- exact GENRE five-value/one-position broadcast to eight destinations;
- logical generation changes across Tonepoet copy, host observation, and terminal paste;
- exact-generation requirement for destructive Cut provenance;
- retained file Copy and Cut followed by `Ctrl+V` with no readable host transport in the reusable picker;
- no-readable-host retained fallback versus present-but-failing authoritative host backend;
- stale asynchronous host-read generation detection;
- direct terminal-paste authority and convergence with metadata parsing;
- existing one-field SingleField versus explicitly selected FieldSet, rowless/missing-key FieldSet application, field-name mapping, overwrite confirmation, context-menu paste, and CUE dimensional semantics remain in the existing focused suite.

## Validation performed in this environment

Passed:

- `git diff --check` equivalent on every modified source file: no whitespace errors.
- 17 source-contract assertions covering broker retention/publication ordering, no-disk identity, no-readable-host fallback, backend-failure behavior, stale-read rejection, write-before-read barrier, terminal-paste authority, filesystem generation binding, ordinary `Ctrl+Shift+V` routing, structured framing/fail-closed parsing, the GENRE regression test, and the headless file Copy/Cut regression test.
- Final archive changed-file accounting and archive extraction verification (performed during packaging).

Not runnable here:

- `cargo` is unavailable.
- `rustc` is unavailable.
- `nix` is unavailable.
- `flox` is unavailable.

Therefore no Rust unit/integration tests, `cargo check`, `cargo test`, Clippy, rustfmt, or full workspace/Nix/Flox gates were executed in this sandbox. The implementation includes the focused Rust regressions but they require an operator environment with the repository's Rust 1.93 toolchain/dependencies.

Recommended operator gates are at minimum `cargo test -p tui-file-picker` and the root Tonepoet clipboard/metadata-focused tests, followed by the repository's normal full workspace gate.

## Scope

No CUE-menu classification, ReplayGain, Analysis, decoding, reference-source lock, or unrelated conversion behavior was intentionally changed.
