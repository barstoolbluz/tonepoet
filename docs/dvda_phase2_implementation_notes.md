# DVD-Audio Phase 2 overlay notes

This bundle implements Phase 2 as a source overlay for tonepoet.

## Decisions encoded in the patch

1. The Phase 1 parser lives in the workspace crate `crates/dvda-phase1/`. The main crate keeps `src/tui/dvda/mod.rs` only as a compatibility re-export (`pub use dvda_phase1::*;`) so existing call sites do not need churn while parser code has one home.
2. `TrackSourceRef::DvdaTrack` carries an explicit `DvdaVolumeSourceRef`, group identity, optional ATS title identity, optional SAMG ordinal, typed decode-boundary fields (`first_pts`, `len_in_pts`, `track_type`, `index_start`, `downmix_matrix`, and ATS title context), resolved structural audio format when known, sector ranges, AOB inventory when the range is ATS-relative, and an explicit `DvdaSectorAddressSpace`. `DvdaVolumeSourceRef` distinguishes user directories, ISO images, and staged AUDIO_TS roots so Phase 3 readers do not infer the backing volume from a plain path. SAMG-only groups use `SamgAbsolute` ranges instead of pretending an ATS mapping exists.
3. Format selection does not trust `track_type` for real discs. Phase 2 records an `audio_format_index` only when one ATS audio format is present. Multi-format ATS tracks keep it as `None`; sample rate, bit depth, expected sample count, and channel-layout extras also remain unknown until Phase 3 packet inspection can identify the stream format.
4. Detection runs after SACD and before CUE/archive fallback. It uses layered evidence: directory `AUDIO_TS/AUDIO_TS.IFO` lookup, UDF lookup inside ISO, ISO9660 bridge lookup inside ISO, and AMG identifier validation at byte offset 0 of the IFO file. Raw ISO scanning is restricted to explicit DVD-Audio requests after filesystem-backed checks fail; normal auto-detection does not route on a stray byte string, and explicit raw evidence does not route unless a backend can also open the volume.
5. ISO materialization treats ISO access as a first-class volume backend. `IsoUdfDvdaVolume` parses UDF descriptors, indexes files inside `AUDIO_TS`, reads IFO/BUP/MKB files on demand, and records AOB size/extent metadata from file entries without staging AOB payload bytes. Phase 2 therefore parses structure directly from the ISO; Phase 3 can reuse the backend for bounded AOB range reads.
6. Default group selection is group 1 when present, otherwise the first parsed group. Explicit group 0 is rejected.
7. MKB/CPPM presence maps to a structured blocked-source result after selected-group structure is emitted into `PreparedTrack` entries; planning and decoding are skipped.
8. DVD-Audio IFO text metadata is not invented. Album and track text fields are left blank; structural values are stored in `extra`.
9. Phase 2 does not require full AOB payload files during materialization. It validates that IFO sector ranges are structurally well formed, records whether the parsed AOB inventory covers each ATS-derived track in `dvda_aob_inventory_covers_track`, and leaves hard AOB coverage/read validation to Phase 3. This allows IFO-only fixture corpora to exercise the structure path.
10. SAMG-only groups are materialized from `AUDIO_PP.IFO` records when no AMG/AOTT or ATSI title references exist. These tracks carry no ATS title identity, use `DvdaSectorAddressSpace::SamgAbsolute`, and record SAMG track metadata in `TrackMetadata.extra`. Phase 3 must implement absolute-sector reading or a stronger SAMG-to-ATS correlation before decoding them.
11. `realize_track()` returns `ConvertError::UnsupportedTrackSource` for `DvdaTrack`; AOB demux/extraction belongs to Phase 3.
12. `PreparedTrack::sample_rate` is now `Option<u32>`. Unknown DVD-Audio rates and non-scalar DVD-Audio rates are represented as `None`; the pipeline no longer smuggles unknown rate through `0`.
13. `PreparedTrack::source_audio` carries typed source-domain audio facts. For DVD-Audio, this includes `SourceAudioCoding::DvdaUnknown`, any primary scalar rate/depth that is structurally valid, and zero or more `ChannelGroupDescriptor` values with per-group sample-rate, bit-depth, channel-count, and channel-assignment facts.
14. `DvdaTrack` keeps core Phase 3 read semantics typed rather than forcing decoders to parse string metadata. ATS-derived tracks carry chapter PTS, length, raw track type, index start, optional downmix matrix, and containing-title PGC context. SAMG-only tracks carry shared timing fields and leave ATS-only fields empty.
15. The fixture-corpus tests live in `src/convert/pipeline/materializer_dvda_fixture_tests.rs` and are included from `materializer_dvda.rs` with `#[cfg(test)]`. They run against `tests/fixtures/dvda` by default, or `DVDA_FIXTURE_ROOT` when an external fixture tree is used. If no fixture root exists, they skip with a diagnostic; if a fixture root exists, they require exactly seven DVD-Audio fixtures and then validate structure materialization, expected PreparedTrack counts from the parsed group model, parser-independent golden expectations from `corpus_probe_output.json`, typed decode-boundary fields, CPPM rejection for MGLETSGETITON/Hawks & Doves/Talking Heads 77, explicit group selection, and track-selection filtering. The golden tests compare exact fixture membership, CPPM state, group counts, selected sector ranges, PTS values, audio facts, and normalized `PreparedSource` snapshots against the probe JSON rather than using the parser model as the test oracle.

16. `DvdaVolumeSourceRef` is the Phase 3 volume-opening contract for DVD-Audio. Directory inputs set `Directory { root }`; ISO inputs set `Iso { path, backend }`, where `backend` is `Udf` or `Iso9660Bridge`, and keep all reads on the detection-proven ISO backend; future extraction fallbacks can set `StagedAudioTs { original, root }` without changing the `DvdaTrack` shape. Pipeline logs and manifest provenance use `original_container()` while decode code can branch on the typed variant.

## Required module declarations

The partial source bundle supplied to the model did not include parent `mod.rs` files. Add these declarations in the real repository if they are not already present:

```rust
// src/tui/mod.rs
pub mod dvda;
```

```rust
// src/convert/pipeline/mod.rs
mod materializer_dvda;
```

If the other materializers are declared as `pub mod`, use the same visibility for `materializer_dvda`.

17. The fixture test suite now uses `tests/fixtures/dvda/corpus_probe_output.json` as a parser-independent golden oracle when that file is present. That file is expected to be produced by an external fixture probe, not by Phase 2 materialization. If a fixture root exists without the probe JSON, the golden tests skip with a diagnostic while the structural tests still run; if the probe exists, corpus membership and exact values are enforced.
18. The current production ISO path does not invoke 7z or a `ToolRunner`; it opens ISO files through direct read backends. The ISO-specific tests therefore exercise confidence-layered detection and confirm raw byte magic does not route auto-detection. A future extraction fallback should add a `StagedAudioTs` volume-source test with a failing/stub ToolRunner.


19. The parser does not present `track_type & 0x07` as an audio-format table index. `AudioChapter` preserves the raw `track_type` byte and records `track_type_low_bits_candidate` only as a diagnostic hint. `AudioTitle` records uniform/distinct low-bit candidates with names that mark them as provisional. Phase 2 derives `DvdaTrack.audio_format_index` only from proven structure, such as an ATS with a single present audio-format entry, and leaves multi-format cases unknown for Phase 3 packet inspection.

### CPPM blocked-source reporting

DVD-Audio CPPM detection now preserves structure. When `DVDAUDIO.MKB` or the parser's
copy-protection flag marks a source as CPPM-protected, the materializer still parses the
disc, selects the requested/default group, builds `PreparedTrack` entries, and returns
`MaterializeError::BlockedSource` carrying a `BlockedSource` payload.

The orchestrator records the parsed `PreparedSource` in the report and emits per-track
`TrackOutcome::Blocked` records under `BlockReason::EncryptedSource`. No plan, realize,
decode, encode, or publish stages run for that source.


### Single parser source of truth

`crates/dvda-phase1/` is the only DVD-Audio parser implementation. `src/tui/dvda/mod.rs` is intentionally a re-export shim. The crate root `crates/dvda-phase1/src/lib.rs` preserves the parser internals' historical `crate::tui::dvda` module path and re-exports the public parser API at the crate root.

The target repository must add `crates/dvda-phase1` to `[workspace].members` and add `dvda-phase1 = { path = "crates/dvda-phase1" }` to the main package dependencies. See `patches/cargo_dvda_phase1_workspace.patch`.


## ISO9660 detection/materialization alignment

The ISO9660 detector no longer exists as a detector-only path. `Iso9660DvdaVolume` implements the same `DvdaVolume` trait as the UDF backend, and `open_dvda_volume_with_detection()` dispatches to the backend named by `DvdaDetection`. If UDF detection wins, materialization opens `IsoUdfDvdaVolume`; if ISO9660 bridge detection wins, materialization opens `Iso9660DvdaVolume`. Explicit raw-magic fallback is diagnostic/low-confidence only and does not make the source a DVD-Audio candidate unless a filesystem-backed `AUDIO_TS/AUDIO_TS.IFO` path is also available.

## Scalar sample-rate migration hardening

The `PreparedTrack::sample_rate` contract now uses `Option<u32>`. This is the right model for DVD-Audio, but it changes a common pipeline field. The bundle therefore adds a compatibility layer rather than expecting every call site to inspect the raw field directly.

Rules for follow-up code:

1. Conversion logic that needs one scalar rate should call `PreparedTrack::scalar_sample_rate()`.
2. Code that can proceed without a rate should handle `None` explicitly.
3. Code that cannot proceed without a rate should call `require_scalar_sample_rate(...)` and surface a typed error or convert that error into the local pipeline error type.
4. DVD-Audio code that needs split channel-group facts should inspect `PreparedTrack::source_audio.channel_groups`, not the scalar rate field.
5. `0` is not a valid unknown-rate value. Historic serialized `0` values now deserialize as `None`.

Run this extra audit before review:

```sh
python3 tools/audit_prepared_track_sample_rate.py .
```

This audit only catches suspicious source reads. It does not replace `cargo fmt`, `cargo check --workspace`, or `cargo test --workspace`.

## DVD-Audio group-selection contract

- Added `DvdaGroupSelection` as the active DVD-Audio group-selection contract. The internal model now supports default behavior, one exact group, all groups, stereo preference, multichannel preference, and highest-resolution preference while preserving the legacy `dvda_group: Option<u8>` field only as a backward-compatible serialized-request fallback.

## Correction: group-global track ordinals vs ATS-local chapter numbers

DVD-Audio ATS chapter numbers restart for each ATS title, while SAMG track numbers describe the flat playback order inside a DVD-Audio group. The Phase 2 materializer now models those address spaces separately:

- `group_track_ordinal` is the 1-based playback ordinal within the selected DVD-Audio group.
- `ats_track_nr` is the ATS-local chapter/track number and is present only for ATSI-derived tracks.
- `samg_track_nr` is the SAMG group track number and is present when `AUDIO_PP.IFO` supplied or correlated that record.

SAMG correlation for ATS-derived tracks now matches on `group_track_ordinal`, not `AudioChapter.track_nr`, so a group that spans multiple ATS titles does not confuse repeated chapter number `1` with group track `1`.

## Real UDF ISO fixture tests

The UDF reader must be validated with real DVD-Audio ISO images, not only with directory fixtures or synthetic ISO9660 images. The fixture tests now discover paired ISO images from `DVDA_ISO_FIXTURE_ROOT`, `DVDA_UDF_ISO_MANIFEST`, or colocated `.iso`/`.img` files under the fixture tree. Set `DVDA_REQUIRE_UDF_ISO_FIXTURES=1` in CI to require all seven real UDF ISO fixtures.

The real-ISO suite verifies:

1. UDF lookup exposes `AUDIO_TS/AUDIO_TS.IFO` and the AMG identifier at file offset 0.
2. Parsing through `IsoUdfDvdaVolume` yields the same navigation summary as parsing the paired directory fixture.
3. UDF AOB byte lengths and extent coverage agree with the parser-independent fixture probe when the probe includes AOB length facts, or with paired directory payload files when available.
4. `AobSectorReader` can read AOB sectors through the UDF backend and match directory payload bytes, including a cross-AOB-file boundary when fixture payloads contain adjacent AOB parts.
5. Multi-extent UDF AOB reads are byte-compared when a real fixture supplies such a file.

The parser crate exposes `UdfAudioTsFileInfo`, `UdfFileExtent`, and `UdfFileStorageKind` so tests and Phase 3 planning can inspect ISO file-size and extent metadata without copying AOB payloads.

## Title reference semantics

AMG/AOTT title references and ATSI fallback references use different identifier spaces. The parser model uses `TitleRefKind` so callers match title references only against the intended field. `AottTitleOrdinal` matches `AudioTitle.title_ordinal`; `AtsPgcTitleNr` matches the raw `AudioTitle.title_nr` PGC identifier. Do not combine those comparisons with a loose `||` matcher.

## Explicit raw DVD-Audio intent

Normal auto-detection still requires filesystem-backed DVD-Audio evidence: a directory, UDF ISO, or ISO9660 bridge path exposing `AUDIO_TS/AUDIO_TS.IFO` with `DVDAUDIO-AMG` at byte offset 0. Raw byte scanning is not auto-route evidence.

When the user explicitly requests DVD-Audio handling and raw AMG evidence exists but no materializable `AUDIO_TS` filesystem path can be opened, `detect_source_kind()` now routes to `SourceKind::DvdAudio`. The materializer returns a DVD-Audio-specific `MaterializeError::Parse` explaining that raw evidence exists but no materializable `AUDIO_TS` path was found. This prevents explicit DVD-Audio intent from falling through to generic `.iso` archive handling.

## Full-workspace acceptance gate for `PreparedTrack::sample_rate: Option<u32>`

Changing `PreparedTrack::sample_rate` from `u32` to `Option<u32>` is intentionally a pipeline-level migration, not just a DVD-Audio change. Reviewers should not accept this overlay on the basis of partial source scans.

After applying the overlay to a full tonepoet checkout, run:

```bash
python3 tools/audit_prepared_track_sample_rate.py .
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
```

The convenience wrapper `tools/verify_dvda_phase2_workspace.sh` runs those checks in order and fails if `cargo` is unavailable. Set `DVDA_ALLOW_FORMAT_WRITE=1` for a local formatting pass that mutates files, and set `DVDA_REQUIRE_UDF_ISO_FIXTURES=1` in CI when the private DVD-Audio ISO fixture corpus is mounted.

The audit is deliberately lexical and conservative. It catches likely pre-migration `PreparedTrack` constructors and direct `.sample_rate` field reads, but it does not replace Rust typechecking. `cargo check --workspace` remains the authority for all call sites outside this overlay.
