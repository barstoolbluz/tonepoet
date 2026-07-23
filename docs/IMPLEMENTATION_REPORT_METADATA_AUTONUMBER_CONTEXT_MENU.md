# Metadata Auto-Numbering Corrective v5 Final Implementation Report

Date: 2026-07-22
Input: `metadata_autonumber_context_menu_world_class_fixed_v4`
Scope: corrective v5 capability reality, reader repair, parser anchoring, persistence-boundary enforcement, deterministic alias normalization, DISK/DISC alias consistency, custom-field preservation, persistence idempotency, and affected tests

## Result

This round aligns auto-numbering with the representations the current production path is authorized to preserve:

- native FLAC/Vorbis and Lofty Vorbis comments: full textual numbering;
- native DSF/ID3, Lofty ID3v2, Lofty APE, and Lofty MP4 ilst: canonical positive unsigned numbering only; and
- unsupported DFF and unclassified Lofty carriers: no numbering capability.

Unsupported numbering is rejected at the persistence boundary. The writer validates before any native carrier mutation, full-file rollback backup, or generic fallback serialization begins. Typed Lofty writers repeat classification against the actual primary tag type, so internal callers cannot bypass the backend invariant.

The final hardening pass also removes two ambiguity classes:

1. Numbering aliases are explicit, backend-scoped, ASCII case-insensitive names after surrounding-whitespace removal. Punctuation is never deleted while deciding whether a key has numbering semantics. Consequently, custom keys such as `T-R-C-K`, `T-R-A-C-K`, `T-R-K-N`, and `TRACK-NUMBER` remain independent metadata.
2. ID3v2, APE, and MP4 changes are normalized as a complete set before mutation. Logical aliases, typed keys, and exact backend-native aliases collapse to one canonical persistence field. Identical operations coalesce; value/value and value/deletion conflicts fail closed with an order-independent result.

The production reader canonicalizes numbering rows from semantic `ItemKey` identity before consulting format-specific display names. MP4 `trkn` and `disk` values are recovered through Lofty's typed numeric accessors when no ordinary tag item populated the row. This repairs empty editor rows without inventing free-form substitutes.

The existing no-op menu policy remains intact. The reported DSF eligibility failure used a one-file row already numbered `1`; `N` was correctly suppressed because it would change nothing. The fixture now starts at `9`, and regression coverage distinguishes an actionable numbering operation from an already-satisfied one.

## Files changed

Relative to the supplied v4 input, the corrective bundle changes:

- `src/metadata_persistence.rs`
- `src/dsf_tags.rs`
- `src/tui/metadata_autonumber.rs`
- `src/tui/command.rs`
- `src/tui/probe.rs`
- `docs/IMPLEMENTATION_REPORT_METADATA_AUTONUMBER_CONTEXT_MENU.md`
- `docs/handoff_manifest.txt` (regenerated last)

The final review-blocker hardening pass itself is confined to `src/metadata_persistence.rs`, `src/dsf_tags.rs`, `src/tui/probe.rs`, this report, and the regenerated manifest. No dependency, lockfile, version, configuration, fixture, or unrelated source changes were made.

## Capability authority and exact alias recognition

`MetadataPersistenceBackend::numbering_capabilities()` owns this exhaustive matrix:

- `NativeFlacVorbis`, `LoftyVorbisComments` -> `TEXTUAL`
- `NativeDsfId3`, `LoftyId3v2`, `LoftyApe`, `LoftyMp4Ilst` -> `PLAIN_UNSIGNED_ONLY`
- `UnsupportedDff`, `UnclassifiedLofty` -> `NONE`

The persistence module owns one semantic numbering-field identity for track number, track total, disc number, and disc total. Logical aliases are an explicit allowlist:

- `TRACKNUMBER`
- `TRACKTOTAL`, `TOTALTRACKS`
- `DISCNUMBER`, `DISKNUMBER`
- `DISCTOTAL`, `DISKTOTAL`, `TOTALDISCS`

Backend-native aliases are recognized only on their own backend:

- ID3v2/DSF ID3: `TRCK`, `TPOS`
- APE: `Track`, `Disc`
- MP4 ilst: `trkn`, `disk`

Matching ignores ASCII case and surrounding whitespace only. It does not remove hyphens, spaces, punctuation, or other characters. The same exact rule governs reader canonicalization, capability validation, persistence-key normalization, and alias cleanup.

The shared editor canonicalizer was narrowed in the same way for numbering totals. Exact `TOTALTRACKS` and `TOTALDISCS` remain recognized aliases; punctuation-bearing forms such as `TOTAL-TRACKS` and `TOTAL-DISCS` remain independent custom fields. Existing non-numbering legacy aliases retain their prior behavior.

## Deterministic complete-change normalization

For ID3v2, APE, and MP4, the writer now normalizes the entire requested change set before touching the carrier:

1. Resolve each exact logical, typed, or backend-native numbering key to its canonical typed persistence key.
2. Normalize deletion semantics and the existing value-trimming policy.
3. Group all operations by canonical persistence key.
4. Coalesce identical operations.
5. Reject any group containing different values or a value/deletion disagreement.
6. Sort canonical groups and diagnostic values so reversal of caller input cannot change the resolved operation or conflict report.
7. Remove parsed aliases semantically, including mixed-case native aliases, while preserving punctuation-bearing custom fields.

This covers logical/native collisions such as `TRACKNUMBER` plus `TRCK`, logical/logical collisions such as `TRACKTOTAL` plus `TOTALTRACKS`, typed/logical collisions, and deletion conflicts. Conflict detection precedes capability rejection and all backup or serialization work, including when the conflicting values are themselves unsupported.

Native FLAC/Vorbis retains its existing complete alias-group normalization. Native DSF already resolves its complete canonical change map before applying changes.

## Reader correction and custom-field preservation

`canonical_editor_fields_from_tag()` no longer derives numbering-row identity solely from `ItemKey::map_key()`. That path produced carrier names such as `TRCK`, `Track`, or atom-oriented names that did not reliably merge into the editor's canonical rows.

The reader now:

1. recognizes typed, logical, and exact backend-native numbering keys semantically;
2. converts canonical number/disc rows to semantic write keys rather than carrying raw aliases back into persistence;
3. preserves textual values recovered from ordinary tag items;
4. supplements only missing rows from `track()`, `track_total()`, `disk()`, and `disk_total()`; and
5. never overwrites an exact textual item with a numeric accessor rendering.

Punctuation-bearing custom fields bypass numbering canonicalization. Real-carrier tests create them through the production writer on ID3v2, APE, and MP4, write genuine numbering metadata beside them, reopen through both Lofty and the production editor reader, and assert that the custom key/value remains independent.

## Persistence-boundary idempotency

The persistence boundary now has an explicit no-op contract rather than relying solely on the auto-numbering planner:

- **Lofty-backed formats:** a semantically satisfied change returns before the fallback hook, full-file backup, or serialization. Repetition tests require zero fallback-hook calls, no rollback marker, and byte-identical carrier bytes.
- **Native FLAC:** comment changes are compared against existing alias cardinality and value before replacement blocks are built. A satisfied request leaves bytes unchanged and publishes no metadata journal; the bounded write claim is released without a residual lock.
- **Native DSF:** the backend compares the semantic tag snapshot before and after applying the resolved changes. A satisfied request returns before tail journaling or carrier mutation and leaves bytes unchanged.

The Lofty full-file path uses the first preparation only as a no-op preflight. After the test hook and rollback copy are armed, it re-reads the carrier and reapplies the normalized change set. It never serializes a stale preflight snapshot if another actor changes unrelated metadata between those stages.

This is a persistence-layer invariant. Callers may still suppress no-ops earlier for efficiency, but correctness no longer depends on them doing so.

## DISK/DISC alias consistency corrective

The final alias-consistency corrective makes the logical numbering table in
`metadata_persistence` the executable authority for every Vorbis and DSF path.
The canonical groups are:

- `DISCNUMBER`, `DISKNUMBER` -> `DISCNUMBER`
- `DISCTOTAL`, `DISKTOTAL`, `TOTALDISCS` -> `DISCTOTAL`

`MetadataNumberingField::from_logical_name()` now searches
`logical_aliases()` rather than restating those spellings in a second match.
Native FLAC, generic Lofty Vorbis, the shared editor reader, and DSF
canonicalization all consume the same exact alias table. Matching ignores only
surrounding whitespace and ASCII case. `DISK-NUMBER`, `DISK-TOTAL`, and other
punctuation-bearing names remain unrelated custom metadata.

Both Vorbis persistence paths canonicalize the complete requested change set
before mutation. A canonical/legacy value conflict, including either
`DISKTOTAL` or `TOTALDISCS`, is rejected before backup, journal, or carrier
mutation. Replacing or deleting a logical disc field removes every physical
alias in its group, then writes at most one canonical field. Native FLAC no-op
detection also requires the surviving physical field to use the canonical
spelling, so a legacy-only field is migrated on the first accepted edit rather
than being mistaken for a fully satisfied canonical state.

The DSF reader canonicalizes legacy `TXXX` descriptions into `DISCNUMBER` and
`DISCTOTAL` rows. The DSF writer resolves conflicts through the same table and
removes all matching extended-text aliases before writing the authoritative
`TPOS` number/total pair. Punctuation-bearing `TXXX` descriptions remain
untouched.

Real-carrier regression tests now prove, for native FLAC, Ogg Vorbis, and DSF,
that:

- legacy `DISK*` fields appear as canonical `DISC*` editor rows;
- an accepted edit removes all legacy aliases and leaves one authoritative
  canonical value;
- `DISCNUMBER`/`DISKNUMBER`, `DISCTOTAL`/`DISKTOTAL`, and
  `DISCTOTAL`/`TOTALDISCS` conflicts fail without changing carrier bytes or
  leaving transactional artifacts;
- `DISK-NUMBER` remains an independent custom field through replacement and
  deletion; and
- repeating an accepted replacement or deletion is a byte-identical no-op.

## Filename parser correction

Explicit/custom side labels may contain one to eight ASCII letters. Filename-derived side numbering is intentionally narrower: the stem must begin with exactly one ASCII side letter followed immediately by a digit.

Therefore:

- `A01 - Come Together.flac` parses as side `A`, sequence `1`;
- `A01/17` remains valid to the generic side parser;
- `trackA01.flac` is rejected as filename evidence;
- `01 - Come Together.flac` is rejected; and
- `SIDE01.flac` is rejected for filename inference while explicit `SIDE01` tag parsing remains supported.

## Tests added or strengthened

The corrective test suite now encodes:

- the exact backend capability matrix and representation classifier;
- exact alias recognition with punctuation-bearing counterexamples;
- production-writer acceptance and preservation of punctuated custom fields on real ID3v2, APE, and MP4 carriers;
- complete ID3v2 and APE round trips for track number, track total, disc number, and disc total through the production writer, carrier reopen, Lofty accessors, and production editor reader;
- absence of recognized free-form numbering substitutes on ID3v2, APE, and MP4;
- fail-closed, byte-identical rejection of `A01`, `01`, `7/17`, and `01/17` for every declared numeric field on ID3v2 and APE;
- MP4 `trkn`/`disk` production writes and editor population for all four fields;
- value/value and value/deletion alias conflicts in both input orders on ID3v2, APE, and MP4;
- deterministic three-way and multiple-field conflict handling;
- equal alias coalescing on all three typed backends;
- byte-identical accepted-write repetition on Vorbis, ID3v2, APE, MP4, native FLAC, and native DSF;
- zero fallback-transaction entry for satisfied Lofty writes;
- stale-preflight protection when a hook changes unrelated carrier metadata;
- DSF plain-number menu eligibility when a change exists and preservation of planner no-op suppression; and
- strict filename anchoring without narrowing explicit/custom tag prefixes;
- exact `DISCNUMBER`/`DISKNUMBER` and
  `DISCTOTAL`/`DISKTOTAL`/`TOTALDISCS` grouping across native FLAC, Lofty
  Vorbis, the shared editor reader, and DSF;
- real-carrier canonical migration and alias deletion on FLAC, Ogg Vorbis, and
  DSF;
- independent rejection of every canonical/legacy disc-number and disc-total
  conflict before mutation; and
- punctuation-safe preservation plus byte-identical repetition for the final
  DISK/DISC corrective.

The Vorbis textual proof continues to cover `A01`, `7`, and `01/17`. DSF coverage includes all four numeric fields, accepted-write repetition, and atomic rejection of lexical numbering.

## Verification performed in this environment

- Verified all 701 original manifest entries before editing.
- Confirmed the source archive contains no absolute or parent-traversal extraction paths.
- Reviewed the scoped diff against the supplied v5 delivery and the original supplied tree.
- Ran diff whitespace/error checks.
- Ran comment/string/raw-string-aware delimiter scans on every changed Rust file.
- Checked changed Rust files for duplicate `#[test]` function names.
- Audited all call sites affected by changed return types and preparation functions.
- Confirmed `Cargo.toml` and `Cargo.lock` are unchanged.
- Validated the committed Ogg, MP3, WavPack, and M4A fixtures with the installed FFmpeg probe.
- Regenerated the self-excluding SHA-256 manifest after every other file change and verified every entry.
- Created the final archive deterministically, reproduced it byte-for-byte, re-extracted it, and compared file content, modes, and symlink targets with the working tree.

## Toolchain limitation

This environment contains no `cargo`, `rustc`, `rustfmt`, or Rust analyzer. Compilation, formatting-tool execution, and Rust test execution are therefore not claimed. The required maintainer-side gates remain:

```text
cargo check --workspace --all-targets
cargo test --workspace --no-fail-fast
```
