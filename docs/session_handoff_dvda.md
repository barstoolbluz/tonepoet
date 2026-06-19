# Session Handoff: DVD-Audio Materializer

## What was accomplished

This session took the DVD-Audio materializer from research brief to working
end-to-end extraction across 7 test discs. The work spans Phases 0-3 of
the implementation plan plus pipeline bug fixes and a design brief for
Phase 4 (TUI disc browser).

### Timeline

1. **Research & planning** — DVD-Audio disc structure, foo_input_dvda
   reference implementation analysis, reasoning model review
2. **Phase 0** — Corpus characterization: 7 DVD-Audio ISOs probed, IFO
   fixtures extracted, diagnostic Python parser, expected properties doc
3. **Phase 1** — Standalone IFO parser crate (`crates/dvda-demuxer/`):
   AMG, ATSI, SAMG parsers, DvdaVolume trait, sector reader
4. **Phase 2** — Pipeline integration: SourceKind::DvdAudio,
   TrackSourceRef::DvdaTrack, DvdaMaterializer, detection/dispatch wiring
5. **Phase 3** — AOB demux + MLP extraction: MPEG-PS demuxer, MLP payload
   extraction, ffmpeg decode, LPCM unpacker, duration validation
6. **Pipeline bug fixes** — CLI convert deadlock, missing PipelineSettings,
   status reporting, post-encode sample drift, CPPM detection
7. **Design brief** — Disc browser & stream selection for TUI/CLI

### Branch

`dvda-phase2-materializer` — 14 commits ahead of main. Main has the 3
pipeline bug fixes cherry-picked.

---

## Key architecture decisions

### DVD-Audio parser lives in a separate crate

`crates/dvda-demuxer/` is a workspace member. The main crate re-exports
it via `src/tui/dvda/mod.rs` (`pub use dvda_demuxer::*;`). Parser code
has one home; changes go in the crate, not the shim.

### Sector-range splitting, not PTS seeking

Track boundaries come from IFO sector ranges (first_sector/last_sector).
No ffmpeg seeking. The MPEG-PS demuxer reads exact sectors. This is how
foo_input_dvda does it.

### MLP decode via ffmpeg subprocess

Raw MLP frames extracted from AOB Private Stream 1 → temp file →
`ffmpeg -f mlp -i temp.mlp -c:a pcm_s32le -f wav output.wav`. The
in-process MPEG-PS demuxer handles sector reading and PES packet
stripping; ffmpeg only does the MLP codec decode.

### LPCM decode in-process

DVD-Audio LPCM unpacking is done in Rust (no ffmpeg needed for PCM).
Handles group1/group2 interleaving, 16/20/24-bit big-endian packing,
different sample rates per group. Based on foo_input_dvda's
`pcm_audio_stream.cpp`. ffmpeg is only used to mux raw PCM → WAV.

### PreparedTrack.sample_rate changed to Option<u32>

DVD-Audio can have unknown sample rates (multi-format ATS where format
is unknowable from IFO alone). The old `u32` sentinel of 0 was replaced
with `Option<u32>`. All existing materializers wrap in `Some()`. Pipeline
consumers use `scalar_sample_rate()` accessor.

### PreparedTrack.source_audio added

New `SourceAudioDescriptor` field carries typed source-domain audio facts
including coding, primary rate/depth, and per-channel-group descriptors
for DVD-Audio's group1/group2 structure.

### CPPM detection probes AOBs, not just MKB

`DVDAUDIO.MKB` presence alone doesn't mean encryption — many ripped ISOs
have decrypted audio with leftover MKB. The parser probes the first AOB
sector for valid MPEG-PS headers (`0x000001BA`). If readable, extraction
proceeds. All 7 test ISOs are decrypted despite 3 having MKB.

### Post-encode validation uses realized WAV as reference

MLP tracks can have sample counts that differ from PTS-derived estimates
(frame boundary rounding). The post-encode lossless validator now probes
the realized PCM WAV and uses its actual sample count as the reference,
not the imprecise PTS estimate.

---

## Key types and where they live

### Pipeline types (`src/convert/pipeline/types.rs`)

- `SourceKind::DvdAudio` — new enum variant
- `TrackSourceRef::DvdaTrack { volume_source, sector_ranges, aob_files, group_nr, first_pts, len_in_pts, ... }` — carries everything Phase 3 needs
- `DvdaVolumeSourceRef` — `Directory { root }`, `Iso { path, backend }`, `StagedAudioTs { original, root }`
- `DvdaSectorAddressSpace` — `AtsAobRelative` or `SamgAbsolute`
- `DvdaGroupSelection` — `Default`, `Group(u8)`, `All`, `PreferStereo`, `PreferMultichannel`, `PreferHighestResolution`
- `SourceOptions.dvda_group: Option<u8>` — legacy field
- `SourceOptions.dvda_group_selection: DvdaGroupSelection` — active selection
- `SourceOptions.dvda_assume_decrypted: bool` — CPPM override (not wired to CLI yet)
- `SourceAudioDescriptor` — typed source audio facts with `ChannelGroupDescriptor`

### DVD-Audio parser (`crates/dvda-demuxer/src/tui/dvda/`)

- `parse_dvda_volume(&volume) -> Result<DvdaDisc>` — main entry point
- `DvdaDisc` — top-level: `amg`, `title_sets`, `samg`, `groups`, `copy_protection`
- `TitleSet` — per ATS: `titles`, `audio_formats[8]`, `aobs[9]`, `downmix_matrices[14]`
- `AudioTitle` — `title_nr` (PGC ID), `title_ordinal` (1-based, matches AOTT), `chapters`
- `AudioChapter` (= `AudioTrack`) — `track_nr`, `track_type`, `sector_ranges`, `first_pts`, `len_in_pts`
- `SectorRange` — `first: u32`, `last: u32` (relative to AOB start)
- `DvdaGroup` — `group_nr`, `title_refs`, `samg_tracks`, `correlation`
- `TitleRef` — `title_set_nr`, `title_nr`, `kind: TitleRefKind` (AottTitleOrdinal vs AtsPgcTitleNr)
- `AobSectorReader` — reads logical sectors from concatenated AOBs with boundary crossing
- `DvdaVolume` trait — `DirectoryDvdaVolume`, `IsoUdfDvdaVolume`, `Iso9660DvdaVolume`
- `CopyProtectionInfo` — `mkb_present`, `cppm_detected`, `aob_probe_readable`
- `refine_copy_protection_from_aob_probe()` — probes first AOB sector for MPEG-PS magic

### Phase 3 modules (`src/convert/pipeline/`)

- `dvda_demux.rs` — MPEG-PS sector demuxer (sector-atomic, safe Rust)
- `dvda_mlp.rs` — MLP payload extraction + ffmpeg bridge
- `dvda_lpcm.rs` — in-process LPCM unpacker (21 channel assignments)
- `dvda_realize.rs` — `realize_dvda_track()` orchestrator
- `dvda_channel_layout.rs` — shared MLP/PCM channel assignment table
- `materializer_dvda.rs` — `DvdaMaterializer` + detection + group selection

### Materializer (`src/convert/pipeline/materializer_dvda.rs`)

- `DvdaMaterializer` implements `Materializer` trait
- `is_dvda_candidate()` — detection: SACD first, then DVD-A (UDF/ISO9660/directory), before archive fallback
- `materialize()` — parse disc, select group, build PreparedTracks, check CPPM
- `open_dvda_volume()` — constructs DvdaVolume from path (directory or ISO)

---

## Known issues and gaps

### track_type does not encode audio format index

The reasoning model's Phase 1 bundle assumed `track_type & 0x07` selects
one of 8 audio format entries. On real discs (MGLETSGETITON), all
track_type low bits are 0 even for titles using format 2. foo_input_dvda
does NOT use track_type for format selection — it determines codec from
AOB packet sub-headers. Phase 3 resolves format from MLP major-sync or
LPCM sub-header after demux.

### No LPCM test discs

All 7 corpus ISOs use MLP. LPCM implementation exists with 348 golden
vectors from foo_input_dvda reference, but no end-to-end test against
a real LPCM DVD-Audio disc.

### CLI flags not wired

`--dvda-group`, `--dvda-assume-decrypted`, `dvda-info` subcommand are
defined in SourceOptions/DvdaGroupSelection but have no clap args in
main.rs. This is Phase 4a — small, ~50-100 lines.

### TUI has no disc browsing

No stream/group picker in the TUI. Phase 4c-d work. Design brief at
`docs/disc_browser_design_brief.md`.

### Pre-existing test compilation errors

6 test files have errors from commits synced from another machine
(preemph, settings_sentinel, tui_format_pipeline_settings). Not caused
by DVD-A work.

---

## Test corpus

At `/mnt/scratch/dev/dawdiolab/test-isos/`:

| Disc | Rate | Ch | Codec | MKB | Tested |
|------|------|----|-------|-----|--------|
| HDAD2009.ISO | 192kHz | 2 | MLP | No | Yes — 2 tracks FLAC |
| AP I Robot | 192kHz | 2 | MLP | No | No (same format as HDAD) |
| AP Friendly Card | 192kHz | 2 | MLP | No | No |
| AP Eye in the Sky | 192kHz | 2 | MLP | No | No |
| MGLETSGETITON | 96kHz | 5 (5.0) | MLP | Yes | Yes — 8 tracks FLAC |
| Hawks & Doves | 176.4kHz | 2 | MLP | Yes | Yes — 9 tracks FLAC |
| Talking Heads 77 | 96kHz | 6 (5.1) | MLP | Yes | Yes — 13 tracks FLAC |

IFO fixtures: `tests/fixtures/dvda/` (7 directories with IFO + MKB files)
AOB sector fixtures: `tests/fixtures/dvda_aob_samples/` (16 sectors per disc)
LPCM reference vectors: `tests/fixtures/dvda_lpcm_foo_reference_vectors.cpp`
Diagnostic parser: `scripts/dvda_corpus_probe.py` (Python, parses IFO fixtures)
Phase 1 crate tests: `cargo test -p dvda-demuxer` (23 tests, all passing)

---

## foo_input_dvda reference

Zip file at repo root: `foo_input_dvda-0.8.2.zip`. Extract with
`unzip foo_input_dvda-0.8.2.zip -d /tmp/foo_input_dvda`. Source is at
`/tmp/foo_input_dvda/src/foo_input_dvda/` after extraction. Key files:

- `ifo.h` — all binary struct definitions (AMG, ATSI, SAMG, sub-headers)
- `dvda_zone.h` / `dvda_zone.cpp` — type hierarchy + IFO parsing logic
- `dvda_block.h` / `dvda_block.cpp` — MPEG-PS sector demuxer (83 lines)
- `pcm_audio_stream.h` / `pcm_audio_stream.cpp` — LPCM unpacker (184 lines)
- `audio_stream_info.h` / `audio_stream_info.cpp` — channel assignment table
- `dvda_filesystem.h` — DvdaVolume trait equivalent
- `mlp_audio_stream.h` — MLP major sync header structure

---

## Design briefs and docs

- `docs/dvda_materializer_brief.md` — original research brief
- `docs/dvd-a-materializer-guidance.md` — reasoning model's Phase 1-5 guidance
- `docs/dvda_corpus_expected.md` — per-disc expected properties
- `docs/dvda_demuxer_reasoning_model_brief.md` — Phase 1 parser brief
- `docs/dvda_phase2_reasoning_model_brief.md` — Phase 2 pipeline brief
- `docs/dvda_phase2_implementation_notes.md` — Phase 2 design decisions
- `docs/dvda_phase3_reasoning_model_brief.md` — Phase 3 demux/extract brief
- `docs/dvda_post_encode_drift_issue.md` — sample drift analysis
- `docs/dvda_cppm_detection_fix.md` — CPPM probe-based detection
- `docs/disc_browser_design_brief.md` — Phase 4 TUI/CLI design
- `docs/cppm_boundary_notes.md` — CPPM product boundary
- `docs/dvda_aob_sector_fixture_validation.md` — AOB fixture validation
- `docs/dvda_extended_corpus_requirements.md` — gaps in test coverage
- `docs/mlp_inspection_reference_notes.md` — MLP frame inspection notes

---

## What the next session should do

### Phase 4a (quick win, no TUI code needed)

Wire CLI flags in `main.rs`:
- `--dvda-group <N|all|stereo|multichannel>` → `SourceOptions.dvda_group_selection`
- `--dvda-assume-decrypted` → `SourceOptions.dvda_assume_decrypted`
- `dvda-info <path>` subcommand → probe ISO, print structure

### Phase 4b-d (TUI work, needs TUI source files loaded)

Read these files to understand the existing TUI architecture:
- `src/tui/app.rs` — AppState, screen management
- `src/tui/browse.rs` — Browse tab file browser
- `src/tui/convert_screen.rs` — Convert screen layout
- `src/tui/context_menu.rs` — context menu system
- `src/tui/draw_overlays.rs` — overlay rendering
- `src/tui/pill.rs` — PillState<T> widget
- `src/tui/keybindings.rs` — key dispatch
- `src/tui/message.rs` — AppMessage enum for async communication
- `src/tui/event_loop.rs` — async event loop

Then implement the disc browser per `docs/disc_browser_design_brief.md`.

### Merge to main

The `dvda-phase2-materializer` branch should be rebased onto current main
and merged once the user verifies everything works. The 3 pipeline fixes
(deadlock, PipelineSettings, status reporting) are already on main via
cherry-pick.
