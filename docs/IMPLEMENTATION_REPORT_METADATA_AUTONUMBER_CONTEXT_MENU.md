# Metadata Auto-Numbering Persistence-Capability Final Correction

Date: 2026-07-22
Input: `metadata_autonumber_context_menu_world_class_fixed_v3`
Scope: the two reported final blockers only; no version, dependency, lockfile, configuration, or unrelated behavior changes

## Result

This revision removes the unused import in the persistence-capability module and adds real-carrier tests that exercise every declared numbering-capable backend through the production metadata writer and production reader. Static analysis confirmed and corrected the suspected MP4 issue: logical editor rows synthesized as `Unknown("TRACKTOTAL")` and related names were not guaranteed to enter Lofty's typed track/disc serialization path.

The prior capability centralization, fail-closed menu and command policy, right-click restoration, parked-editor rendering, Custom overlay, source-derived sequences, strict prefixes, raw `TRACKNUMBER` filename carriage, dirty/save behavior, and atomic in-memory mutation are preserved.

## Files changed in this corrective round

- `src/metadata_persistence.rs`
- `src/tui/probe.rs`
- `tests/fixtures/metadata_persistence/README.md`
- `tests/fixtures/metadata_persistence/vorbis.ogg`
- `tests/fixtures/metadata_persistence/id3v2.mp3`
- `tests/fixtures/metadata_persistence/ape.wv`
- `tests/fixtures/metadata_persistence/mp4.m4a`
- `docs/IMPLEMENTATION_REPORT_METADATA_AUTONUMBER_CONTEXT_MENU.md`
- `docs/handoff_manifest.txt` (regenerated after every other file)

The fixture directory is also visible through the repository's existing `crates/dvda-demuxer/tests/fixtures` symlink; that is not a duplicate file set or a changed symlink.

## Warning correction

`src/metadata_persistence.rs` now imports only:

```rust
use lofty::file::TaggedFileExt;
```

`AudioFile` is not imported there. The production writer in `src/tui/probe.rs` still imports `AudioFile` because `TaggedFile::save_to_path` is supplied by that trait.

## Persistence-boundary writer correction

The authoritative persistence module now also exposes the UI-neutral:

```rust
normalize_numbering_item_key_for_backend(backend, key)
```

Core editor rows may exist before a carrier contains the corresponding field, so their logical keys can be `ItemKey::Unknown("TRACKNUMBER")`, `TRACKTOTAL`, `DISCNUMBER`, or `DISCTOTAL`. Before the generic Lofty writer mutates ID3v2, APE, or MP4 ilst metadata, it now normalizes those logical names to:

- `ItemKey::TrackNumber`
- `ItemKey::TrackTotal`
- `ItemKey::DiscNumber`
- `ItemKey::DiscTotal`

The writer removes both the logical and typed spellings, then inserts only the typed key. This preserves deletion semantics, cleans up any earlier free-form spelling, and ensures MP4 conversion produces the standard `trkn` and `disk` pair structures. Vorbis comments retain their existing canonical alias writer. Unclassified backends retain their fail-closed capability status and do not receive invented typed-key behavior.

This normalization is adjacent to and consumed by the production persistence boundary; it does not introduce TUI concepts into low-level code.

## Policy tests versus persistence evidence

The existing tests in `src/metadata_persistence.rs` remain **policy and mapping tests**. They prove that every backend variant has an explicit capability declaration, unknown variants fail closed, and synthetic numbering keys normalize only for the intended typed serializers. They do not claim to prove disk persistence by themselves.

The new tests in `src/tui/probe.rs` are **production-path persistence round-trip tests**. When executed, they copy committed real carriers to temporary paths, call the public production `write_all_tags()` entry point, reopen through the production editor reader, and independently inspect the resulting Lofty or native metadata representation. They are committed and statically reviewed in this environment; their runtime results are not claimed here.

### Real-carrier coverage

- **Native FLAC/Vorbis:** the pre-existing `saved_side_prefixed_flac_reopens_materializes_and_renders_exact_output_path` integration test saves `A01` through the production writer, reopens it, materializes conversion metadata, and asserts the exact `A01 - Come Together` output path.
- **Lofty Vorbis comments:** a real Ogg/Vorbis carrier round-trips `A01`, `7`, and `01/17` exactly.
- **Lofty ID3v2:** a real MP3/ID3v2 carrier round-trips `A01`, `7`, and `01/17` exactly through the standard track-number item, with no free-form `TRACKNUMBER` substitute.
- **Lofty APE:** a real WavPack/APE carrier round-trips `A01`, `7`, and `01/17` exactly through the standard track-number item, with no free-form substitute.
- **Lofty MP4 ilst:** a real M4A carrier receives logical editor keys for track number/total and disc number/total. Reopen asserts `7/17` and `2/3` through Lofty's typed accessors, all four canonical editor values, no `Unknown` substitutes, and no synthetic logical identifiers in the carrier bytes.
- **Native DSF/ID3:** a generated valid DSF fixture round-trips all four plain numeric fields. A subsequent `A01` write is rejected and the file remains byte-identical.
- **Unknown carrier:** both capability resolution and the production writer reject an invalid carrier; the file remains byte-identical.

The committed Ogg, MP3, WavPack, and M4A fixtures are tiny valid audio carriers. Tests mutate temporary copies only. Their generation commands and tool version are recorded in `tests/fixtures/metadata_persistence/README.md`; FFmpeg is not required when the Rust tests run.

## Atomicity and existing policy

The correction does not relax capability enforcement. DSF and MP4 remain plain-unsigned-only in the auto-numbering capability model. Textual backends remain authorized only for the representations their real-carrier tests exercise. Unknown and unsupported carriers remain fail closed. Menu, command, Custom-overlay, and direct mutation callers continue to converge on the same execution-time validation.

The writer normalization occurs only after the existing file-scoped rollback path is armed. Native DSF lexical rejection occurs before commit, and its new test verifies byte-exact non-mutation.

## Performance note

The optional classifier-cache redesign was deliberately not included. It would require editor-model lifetime and staleness policy beyond this narrowly scoped final correction. Menu construction therefore retains authoritative synchronous probing, and execution retains independent validation.

## Static verification performed

- Compared the corrected tree against the supplied v3 tree and confirmed the source diff is limited to the persistence module, production writer/tests, real-carrier fixtures, this report, and the regenerated manifest.
- Inspected every caller of `normalize_numbering_item_key_for_backend` and the complete generic writer mutation path.
- Confirmed the unused `AudioFile` import is absent from `src/metadata_persistence.rs`.
- Confirmed `Cargo.toml`, `Cargo.lock`, version declarations, and unrelated source files are unchanged.
- Validated all committed carrier fixtures with the installed FFmpeg probe.
- Ran delimiter/string/comment/raw-string-aware structural scans on every changed Rust file.
- Ran duplicate-struct-field, duplicate-test-name, import-use, whitespace, line-ending, final-newline, archive-path, and scoped-diff checks.
- Regenerated and verified the 700-entry self-excluding SHA-256 manifest after all source, fixture, and report changes.
- Extracted the deterministic final archive into a fresh directory and compared it byte-for-byte with the working tree.

## Remaining limitation

This container has no `cargo`, `rustc`, `rustfmt`, or `nix`, and no usable toolchain download path. Therefore this report does not claim compilation, `cargo fmt --check`, the Nix build, or execution of `cargo test --workspace --no-fail-fast`. The new tests are committed and statically reviewed but remain mandatory maintainer-side execution gates.
