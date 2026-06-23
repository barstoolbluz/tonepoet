# Blu-ray Audio Extraction Plan for tonepoet

## Recommendation

Use **oxideav-bluray + oxideav-mpegts as the primary implementation path**, with a backend adapter from the start so tonepoet can add a `libbluray-sys` fallback later if validation shows unacceptable disc compatibility problems.

Do **not** start with `libbluray-sys`.

This recommendation changed slightly from the initial draft:

1. Add `oxideav-bluray` with `default-features = false` during Phase 0. Its current default features include AACS support and an online VUK derivation path. tonepoet should keep AACS out of the baseline implementation and make it an explicit Phase 6 feature.
2. Add the backend seam immediately in Phase 0, not later. The seam keeps the early oxideav implementation honest and gives us a clean insertion point for `libbluray-sys` if the validation corpus exposes parser or title-reading failures.
3. Do not assume a stable high-level `open_title_chapters` API. Use `Disc::chapters`, `Disc::title_streams`, `Disc::open_title_with_angle`, `TitleSource::seek_to`, `TitleSource::pts_continuity_segments`, and `TitleSource::map_clip_pts_to_title_pts` where available. If the pinned crate version exposes a chapter-segment convenience helper, wrap it behind the backend trait as an optimization.
4. Treat LPCM bit depth as a Phase 0/1 risk item, not a Phase 3 surprise. The high-level stream catalogue gives codec, PID, language, sample rate, and channel count, but bit depth may require CLPI inspection or PES-header probing.

The core reason to prefer oxideav is that tonepoet is an audio extraction tool, not a Blu-ray playback engine. We do not need BD-J menus, interactive navigation, graphics overlays, live playback, or advanced player state. We need:

* open ISO or BDMV directory
* enumerate authored playlists
* identify primary audio streams
* read the selected title byte stream
* demux the selected PID
* extract or decode the selected audio stream
* map chapters to output tracks
* preserve tonepoet’s existing sidecar and MusicBrainz workflow

That scope aligns well with the oxideav crates. `libbluray` has much broader real-disc maturity, but `libbluray-sys` would require tonepoet to write and audit a safe Rust wrapper before implementing the extraction feature itself. That wrapper would need to handle pointer ownership, title-info allocation/freeing, read/seek state, runtime library discovery, Nix wiring, and error conversion. Starting with that path would delay the actual tonepoet integration.

## Decision matrix

| Criterion                     | oxideav-bluray + oxideav-mpegts                                                                           | libbluray-sys                                                                                                                        |
| ----------------------------- | --------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| Disc compatibility            | Highest risk. Very new code, lightly field-tested. Must validate against real discs.                      | Strongest advantage. libbluray has many years of playback use.                                                                       |
| Fit for tonepoet architecture | Strong. Pure Rust, typed API, similar to existing vendored DVD-Video direction.                           | Mixed. Powerful C library, but raw unsafe bindings do not match tonepoet’s safe internal style yet.                                  |
| Build integration             | Strong if added with `default-features = false`. Cargo-only baseline.                                     | Acceptable under Nix, but adds system C libraries and bindgen behavior.                                                              |
| API ergonomics                | Strong. `Disc`, `TitleInfo`, `TitleSource`, stream catalogues, chapter marks, CLPI parsing, TS stripping. | Low at the Rust boundary. tonepoet would need to design the safe wrapper.                                                            |
| AACS path                     | Available later via `oxideav-aacs`, but should be feature-gated and validated separately.                 | Available through libaacs/libbdplus linkage, but still requires external libraries and key material.                                 |
| Licensing                     | MIT.                                                                                                      | LGPL-2.1-or-later for the sys crate and libbluray. Compatible with GPL-3.0-or-later, but adds dynamic/system-library considerations. |
| Fallback strategy             | Good. Start here and fallback if concrete tests fail.                                                     | Good as a second backend, but expensive as the first step.                                                                           |

## Final policy

Build the Blu-ray feature on oxideav first, but never spread direct oxideav calls throughout the pipeline. All oxideav usage must live behind:

```rust
disc/bluray_backend.rs
disc/bluray_backend_oxideav.rs
disc/bluray_utils.rs
```

Add `libbluray-sys` only if validation triggers it.

Fallback triggers:

1. Valid unencrypted BDMV/ISO sources fail to mount or enumerate titles at an unacceptable rate.
2. Multi-clip playlists produce wrong chapter durations, wrong clip boundaries, or corrupt demux output.
3. Audio stream PID, language, codec, or channel selection proves unreliable across real discs.
4. LPCM bit depth cannot be recovered reliably through CLPI or PES-header probing.
5. AACS support proves unusable for realistic KEYDB workflows after the unencrypted path works.
6. UHD-BD/UDF edge cases become a near-term support target and oxideav cannot parse them reliably.

## Architecture to follow from DVD-Video

The included DVD-Video files establish the pattern:

* `dvdv_utils.rs`: detection, open, parse, sidecar overlay
* `dvdv_mapper.rs`: parsed disc model to `DiscContents`
* `materializer_dvdv.rs`: selected program to `PreparedSource` / `PreparedTrack`
* `dvdv_realize.rs`: selected track source to WAV
* `probe.rs`: format-specific probe before ffmpeg fallback
* `model.rs`: `DiscFormat`, `PresentationId`, `DiscPresentation`, `DiscTrack`
* `command.rs`: sidecars and MusicBrainz synthetic TOC

Blu-ray should follow that same layout:

```text
disc/bluray_backend.rs
disc/bluray_backend_oxideav.rs
disc/bluray_utils.rs
disc/bluray_mapper.rs
convert/materializer_bluray.rs
convert/bluray_realize.rs
```

Add these model and pipeline variants:

```rust
DiscFormat::BluRay

PresentationId::BluRayTitle {
    playlist_number: u32,
    audio_pid: u16,
    audio_stream_index: u8,
    angle_number: u8,
}

SourceKind::BluRay

TrackSourceRef::BluRayTrack {
    source: PathBuf,
    playlist_number: u32,
    title_index: usize,
    angle_number: u8,
    chapter_number: u32,
    chapter_start_pts_90k: u64,
    chapter_end_pts_90k: Option<u64>,
    audio_pid: u16,
    audio_stream_index: u8,
    audio_coding: BluRayAudioCoding,
    sample_rate: Option<u32>,
    bit_depth: Option<u32>,
    channels: Option<u8>,
    channel_layout: Option<String>,
}
```

Use `playlist_number + audio_pid + audio_stream_index + angle_number` as the durable presentation identity. `title_index` may help call the current backend, but it should not become the persisted identity because filtered title ordering can change.

Define:

```rust
enum BluRayAudioCoding {
    Lpcm,
    Ac3,
    Eac3,
    Dts,
    TrueHd,
    DtsHd,
    DtsHdMaster,
}
```

Implement:

```rust
impl BluRayAudioCoding {
    fn label(self) -> &'static str;
    fn is_lossless(self) -> bool;
    fn codec_rank(self) -> u8;
    fn elementary_extension(self) -> &'static str;
    fn ffmpeg_format_hint(self) -> Option<&'static str>;
}
```

Suggested codec rank:

```text
LPCM          7
TrueHD        6
DTS-HD MA     5
DTS-HD HR     4
DTS           3
E-AC-3        2
AC-3          1
```

## Backend trait

Add the trait before writing mapper/materializer logic:

```rust
pub trait BlurayBackend {
    type Disc;
    type TitleSource: std::io::Read + std::io::Seek;

    fn open(path: &Path) -> Result<Self::Disc, String>;

    fn disc_label(disc: &Self::Disc, source: &Path) -> Option<String>;

    fn titles(disc: &Self::Disc) -> Result<Vec<BlurayTitleInfo>, String>;

    fn title_by_playlist(
        disc: &Self::Disc,
        playlist_number: u32,
    ) -> Result<BlurayTitleKey, String>;

    fn chapters(
        disc: &Self::Disc,
        title: BlurayTitleKey,
        angle_number: u8,
    ) -> Result<Vec<BlurayChapterInfo>, String>;

    fn streams(
        disc: &Self::Disc,
        title: BlurayTitleKey,
    ) -> Result<Vec<BlurayAudioStreamInfo>, String>;

    fn max_angle(
        disc: &Self::Disc,
        title: BlurayTitleKey,
    ) -> Result<u8, String>;

    fn open_title(
        disc: &Self::Disc,
        title: BlurayTitleKey,
        angle_number: u8,
        decryptor: Option<&mut dyn BlurayStreamDecryptor>,
    ) -> Result<Self::TitleSource, String>;

    fn pts_continuity_segments(
        source: &Self::TitleSource,
    ) -> Result<Vec<BlurayPtsContinuitySegment>, String>;
}
```

Keep backend structs tonepoet-owned. Do not leak oxideav types into `DiscContents`, `PreparedTrack`, `SourceOptions`, sidecars, or realizer signatures.

## Phase 0 — Crate integration and backend spike

### Goal

Prove that the oxideav crates compile in tonepoet’s Rust/Nix environment and that the backend adapter can open and enumerate a known Blu-ray source.

### Dependencies

Add exact, pinned dependencies first:

```toml
oxideav-bluray = { version = "=0.0.3", default-features = false }
oxideav-mpegts = { version = "=0.0.2", default-features = false }
```

Do not enable `oxideav-bluray` default features in Phase 0. In particular, keep AACS and online VUK derivation out of the baseline.

Prepare but do not enable:

```toml
oxideav-aacs = { version = "=0.1.3", default-features = false, optional = true }
```

Add a future feature:

```toml
bluray-aacs = ["dep:oxideav-aacs", "oxideav-bluray/aacs"]
```

Do not include `oxideav-bluray/aacs-online` by default. Add it only as a separate explicitly named feature if tonepoet later wants that behavior.

### Work items

1. Add `disc/bluray_backend.rs`.
2. Add `disc/bluray_backend_oxideav.rs`.
3. Add a backend smoke test or ignored integration test that:

   * opens a known unencrypted BDMV directory or ISO
   * prints or asserts title count
   * prints playlist number, duration, angle count, chapter count
   * prints audio stream PID, stream index, codec, language, sample rate, channel count
   * attempts to recover LPCM bit depth through CLPI or PES-header probing
4. Validate Rust version compatibility. The oxideav crates currently require Rust 1.80, so tonepoet’s toolchain must meet or exceed that.
5. Add a local `BlurayAudioCoding` mapper from oxideav stream coding types.
6. Add a local LPCM PES-header parser test fixture for the 4-byte BD-ROM LPCM audio header.
7. Add a capability probe in logs:

   * title enumeration supported
   * stream catalogue supported
   * chapter marks supported
   * PTS continuity map supported
   * CLPI stream coding info accessible
   * LPCM bit depth discovered or not discovered

### Exit criteria

* `cargo check` passes in the Nix dev shell.
* The crate additions do not pull AACS or online VUK behavior into the baseline build.
* One known unencrypted Blu-ray source opens.
* The backend adapter can enumerate titles, chapters, angles, and audio streams.
* The smoke test records whether LPCM bit depth is recoverable before realization.
* Existing SACD, DVD-Audio, and DVD-Video behavior remains unchanged.

### Stop condition

If Phase 0 cannot open and enumerate a normal unencrypted BDMV/ISO source, stop and evaluate a `libbluray-sys` backend before building the mapper/materializer.

## Phase 1 — Detection and browsing

### Goal

Make Blu-ray sources appear in the existing disc browser model, parallel to DVD-Video.

### Work items

1. Add detection helpers:

   * `is_bluray_source(path)`
   * `is_bluray_iso(path)`
   * `is_bluray_directory(path)`
   * `bluray_directory_root(path)`
2. Directory detection should accept:

   * disc root containing `BDMV/`
   * the `BDMV/` directory itself, resolving back to the disc root
3. ISO detection should try `BlurayBackend::open` and reject false positives cleanly.
4. Add `DiscFormat::BluRay`.
5. Add `PresentationId::BluRayTitle`.
6. Add display helpers:

   * user-facing stream numbers should be one-based
   * persisted stream indexes should stay zero-based
   * PID should display as hex and decimal when useful
7. Implement `map_bluray_source(path) -> DiscContents`.
8. Implement `bluray_mapper.rs`:

   * one `DiscPresentation` per playlist/audio-stream/angle combination
   * one `DiscTrack` per chapter
   * suppress titles with no chapters
   * suppress titles with no supported audio streams
   * suppress very short menu/intro playlists unless explicitly requested
   * include suppression reasons
9. Add copy-protection summary:

   * unencrypted or unknown in Phase 1
   * AACS/BD+ detected but unsupported if discoverable
10. Add `probe_bluray_disc()` in `probe.rs`.
11. Route `probe_audio()` in this order:

* DVD-Audio first
* DVD-Video next, preserving current hybrid handling
* Blu-ray next
* SACD
* ffmpeg fallback

The exact order can change if tonepoet already has a central source detector, but Blu-ray must run before generic ffmpeg probing.

### Presentation label

Use labels like:

```text
Blu-ray Playlist 00012 Stream 1 PID 0x1100 · LPCM 96 kHz / 24-bit / Stereo
Blu-ray Playlist 00003 Stream 2 PID 0x1101 · TrueHD 48 kHz / 5.1
Blu-ray Playlist 00007 Stream 1 PID 0x1100 · DTS-HD MA 96 kHz / 5.1 · Angle 1/2
```

### Browsing score

Sort default presentations by:

1. has computable chapter durations
2. chapter count
3. total duration
4. stereo preference
5. lossless preference
6. codec rank
7. sample rate
8. bit depth
9. lower playlist number
10. lower audio stream index
11. lower angle number

This mirrors DVD-Video: identify the likely main program first, then prefer the best audio stream.

### Exit criteria

* Convert view detects a Blu-ray source before ffmpeg fallback.
* Disc browser lists useful Blu-ray presentations.
* Presentation identity survives serialization/deserialization.
* Default presentation selection is deterministic.
* Suppressed playlists explain why they were hidden.
* Existing DVD-Video presentation labels and scoring remain unchanged.

## Phase 2 — Materializer

### Goal

Convert a selected Blu-ray presentation into `PreparedSource` with one `PreparedTrack` per selected chapter.

### Work items

1. Add `BlurayMaterializer`.
2. Add Blu-ray fields to `SourceOptions`:

   * `bluray_playlist: Option<u32>`
   * `bluray_audio_pid: Option<u16>`
   * `bluray_audio_stream: Option<u8>`
   * `bluray_angle: Option<u8>`
3. Add `explicit_bluray_requested()`.
4. Add `is_bluray_candidate(req)`.
5. Route Blu-ray materialization after existing disc-type first-refusal logic and before generic file handling.
6. Open the disc through `BlurayBackend`.
7. Select title/stream/angle using explicit options or scoring.
8. Validate:

   * playlist exists
   * audio PID exists in selected title
   * stream index and PID agree if both are supplied
   * angle is one-based and within range
   * selected chapters exist
9. Create one `PreparedTrack` per selected chapter:

   * `source_ordinal = chapter_number`
   * `track_number = output order`
   * `TrackSourceRef::BluRayTrack`
   * `sample_rate`
   * `bit_depth`
   * `channels`
   * `SourceAudioDescriptor`
10. Carry both chapter PTS and title identity:

* chapter start PTS in 90 kHz units
* chapter end PTS from next chapter or title duration if known
* playlist number
* title index/key
* angle number
* PID

11. Load and overlay Blu-ray metadata sidecars if present.
12. Add provenance:

* `oxideav-bluray`
* `oxideav-mpegts`
* `ffmpeg` only when selected compressed codecs require it

### LPCM bit-depth rule

For LPCM, materialization must either determine bit depth or fail with a targeted error before realization.

Use this order:

1. CLPI `StreamCodingInfo` if the pinned oxideav API surfaces enough data.
2. First matching LPCM PES header for the selected PID.
3. A clear `MaterializeError::Parse` explaining that LPCM bit depth could not be determined.

Do not allow `bit_depth = None` for LPCM tracks that reach the realizer.

### Exit criteria

* Blu-ray materializer creates deterministic `PreparedTrack` values.
* Explicit playlist/PID/angle options work.
* Invalid explicit selections fail with actionable errors.
* LPCM tracks have sample rate, channel count, and bit depth before realization.
* Compressed tracks carry enough info for ffmpeg decode.
* Sidecar overlay works for selected tracks.

## Phase 3 — Realizer, LPCM path

### Goal

Extract Blu-ray LPCM in-process to WAV without ffmpeg.

### Work items

1. Add `realize_bluray_track()`.
2. Match only `TrackSourceRef::BluRayTrack`.
3. For `BluRayAudioCoding::Lpcm`:

   * open selected title/angle through `BlurayBackend`
   * seek to selected chapter start when possible
   * read clean 188-byte TS packets
   * demux only the selected audio PID with `oxideav-mpegts`
   * reassemble PES packets
   * map PES PTS to title PTS using the backend PTS-continuity data when needed
   * include PES packets whose title PTS falls inside the chapter interval
   * parse and validate the 4-byte BD-ROM LPCM PES payload header
   * strip the header
   * write PCM payload to WAV
4. Implement BD-ROM LPCM header parsing:

   * audio presentation type
   * sampling frequency
   * bits per sample
   * channel assignment
5. Validate packet header values against materializer assertions.
6. Convert big-endian LPCM samples to little-endian WAV samples.
7. Support:

   * 48 kHz
   * 96 kHz
   * 192 kHz
   * 16-bit
   * 20-bit if the WAV writer can represent valid bits cleanly
   * 24-bit
   * stereo
   * 5.1
   * 7.1 where channel mapping is known
8. Use WAVE_FORMAT_EXTENSIBLE for:

   * more than two channels
   * valid bits different from container bits
   * channel masks when known
9. Use scoped temp files and atomic publish behavior matching DVD-Video.
10. Add cancellation checks in the TS read loop.
11. Add post-write validation:

* non-empty WAV
* expected sample rate
* expected channels
* expected bit depth or valid bits

### PTS and chapter-boundary rule

Do not rely only on byte offsets for multi-clip playlists. Blu-ray PlayItems can restart or shift STC timelines. Use the title-level PTS-continuity map to determine whether a PES belongs to the selected chapter.

If PTS is missing on an early packet, keep a small pre-roll buffer and attach the packet to the first following in-range timestamp only when the packet belongs to the selected PID and continuity is otherwise valid.

### Exit criteria

* LPCM stereo exports to valid WAV.
* LPCM multichannel exports to valid WAV where channel mapping is supported.
* 48/96/192 kHz fixtures pass.
* 16/24-bit fixtures pass.
* 20-bit either passes with valid-bits handling or fails with a specific TODO error.
* Multi-clip playlist extraction does not mix chapters or clips incorrectly.
* DVD-Video LPCM tests still pass.

## Phase 4 — Realizer, compressed codec path

### Goal

Extract compressed Blu-ray audio by PID and decode through tonepoet’s existing ffmpeg `ToolRunner`.

### Work items

1. For non-LPCM codecs:

   * open selected title/angle
   * demux selected PID with `oxideav-mpegts`
   * reassemble PES
   * map PTS to title time when needed
   * keep only selected chapter packets
   * write an elementary scratch file
2. Use codec-specific scratch extensions:

   * AC-3: `.ac3`
   * E-AC-3: `.eac3`
   * DTS / DTS-HD / DTS-HD MA: `.dts`
   * TrueHD: `.thd`
3. Decode via `ToolRunner`, never direct process spawning.
4. Prefer ffmpeg autodetection for DTS-HD and TrueHD unless a required demuxer format hint proves reliable.
5. Output WAV as `pcm_s32le`, consistent with existing compressed-codec behavior.
6. Preserve stderr capture, timeouts, cancellation, and concurrency limits.
7. Validate non-empty WAV.
8. Add fallback mode:

   * write a chapter-bounded `.m2ts` segment
   * call ffmpeg with explicit stream mapping
   * keep this fallback local to Blu-ray compressed realization
   * log that fallback was used

### Why keep a fallback

Some Blu-ray compressed formats may require framing details that elementary extraction loses or exposes differently than ffmpeg expects. The normal path should be PID/PES elementary extraction, but the `.m2ts` fallback gives us a practical escape hatch without switching the whole architecture to ffmpeg-first extraction.

### Exit criteria

* AC-3 decodes through ffmpeg.
* DTS decodes through ffmpeg.
* E-AC-3 decodes through ffmpeg when fixture coverage exists.
* TrueHD decodes through ffmpeg on at least one fixture.
* DTS-HD MA decodes through ffmpeg on at least one fixture.
* Scratch files clean up on success and failure.
* LPCM still bypasses ffmpeg.

## Phase 5 — Metadata, sidecars, and MusicBrainz

### Goal

Give Blu-ray the same metadata workflow as DVD-Video.

### Sidecar identity

Use playlist, PID, stream index, and angle:

```toml
[source]
sidecar_kind = "blu_ray"

[source.presentation]
playlist_number = 12
audio_pid = 4352
audio_stream_index = 0
angle_number = 1
chapter_count = 10
duration_fingerprint = "..."
```

### Work items

1. Add Blu-ray sidecar structs parallel to DVD-Video:

   * `BluRayMetadataSidecar`
   * `BluRayMetadataSource`
   * `BluRayPresentationIdentity`
   * `BluRayMetadataTrack`
2. Add helpers:

   * `bluray_metadata_sidecar_path_for_source`
   * `load_bluray_metadata_sidecar_presentations`
   * `save_bluray_metadata_sidecar`
   * `overlay_bluray_sidecar_metadata`
3. Preserve unknown keys when editing existing sidecars.
4. Overlay sidecar metadata during:

   * browsing
   * materialization
   * metadata editor preload
5. Add MusicBrainz synthetic TOC helpers:

   * `bluray_source_to_cd_sectors`
   * `bluray_presentation_to_cd_sectors`
   * `bluray_editor_durations_to_cd_sectors`
6. Use selected presentation only for MusicBrainz TOC generation.
7. Reject incomplete, zero, negative, or obviously wrong durations.
8. Add text-search fallback in the metadata editor, parallel to DVD-Video behavior.
9. Add default presentation scoring for metadata lookup:

   * prefer sidecar metadata
   * prefer complete positive durations
   * prefer chapter count
   * prefer duration
   * prefer stereo/lossless/audio quality only after main-program signals

### Exit criteria

* Blu-ray browser shows sidecar album and track metadata.
* Metadata editor can save and reload a selected Blu-ray presentation.
* MusicBrainz lookup works from selected chapter durations.
* Sidecars for different playlists or audio streams do not collide.
* DVD-Video sidecar behavior remains unchanged.

## Phase 6 — Optional AACS support

### Goal

Support encrypted discs when users provide lawful key material and explicitly enable the feature.

### Build policy

AACS must be feature-gated:

```toml
bluray-aacs = [
    "dep:oxideav-aacs",
    "oxideav-bluray/aacs",
]
```

Do not enable `aacs-online` by default. If tonepoet wants that later, add a separate feature:

```toml
bluray-aacs-online = [
    "bluray-aacs",
    "oxideav-bluray/aacs-online",
]
```

### Work items

1. Add `BlurayStreamDecryptor` abstraction to tonepoet’s backend layer.
2. Integrate oxideav’s decryptor path behind the backend.
3. Support KEYDB discovery:

   * oxideav default behavior
   * tonepoet config override
   * environment override if useful
4. Add copy-protection summary states:

   * no encryption detected
   * AACS detected, keys available
   * AACS detected, keys missing
   * BD+ detected, unsupported
   * unknown protection status
5. Add user-facing errors:

   * encrypted disc support not enabled
   * KEYDB not found
   * disc key missing
   * volume ID unavailable
   * BD+ unsupported
6. Keep unencrypted behavior identical with and without the feature.
7. Add test mode using synthetic or mock fixtures where possible.

### Exit criteria

* Unencrypted discs work when `bluray-aacs` is disabled.
* Unencrypted discs work when `bluray-aacs` is enabled.
* AACS discs fail clearly when key material is missing.
* A known KEYDB-backed disc can decrypt in a controlled test environment.
* tonepoet does not claim BD+ support unless implemented and validated.

## Validation plan

Use three fixture tiers.

### Tier 1 — Synthetic fixtures

* Minimal BDMV directory with one playlist, one clip, one LPCM stream.
* Generated 188-byte TS packets for demux unit tests.
* PES reassembly fixture for selected PID.
* BD-ROM LPCM header parser fixtures.
* WAV byte-order fixtures.
* PTS continuity fixtures with a simulated multi-PlayItem title.

### Tier 2 — Authored open fixtures

* Unencrypted Blu-ray folder or ISO with one playlist and one clip.
* Multi-chapter concert-style playlist.
* Multi-clip playlist.
* LPCM stereo.
* LPCM multichannel.
* AC-3.
* DTS.
* TrueHD or DTS-HD MA if available.

### Tier 3 — Real-disc corpus

Use legally available private test discs:

* concert Blu-ray with LPCM
* movie Blu-ray with TrueHD
* movie Blu-ray with DTS-HD MA
* multi-angle disc if available
* seamless-branching or multi-clip disc
* AACS disc only for Phase 6

Track each source through this matrix:

```text
detect
mount
titles
chapters
streams
bit_depth
materialize
lpcm_demux
compressed_demux
ffmpeg_decode
wav_validate
metadata_overlay
musicbrainz_toc
```

## Implementation order summary

1. **Phase 0:** dependencies, no-default-features, backend trait, oxideav adapter, smoke test.
2. **Phase 1:** detection, mapper, `DiscContents`, browser/probe integration.
3. **Phase 2:** materializer, selected playlist/PID/angle/chapter representation, bit-depth preflight.
4. **Phase 3:** in-process LPCM TS/PES demux to WAV.
5. **Phase 4:** compressed-codec PID extraction and ffmpeg decode.
6. **Phase 5:** sidecars, metadata editor, MusicBrainz synthetic TOC.
7. **Phase 6:** optional AACS.
8. **Fallback phase only if validation triggers it:** `libbluray-sys` backend.

## `libbluray-sys` contingency plan

Do not mix backends in the first implementation. If fallback triggers fire, add:

```rust
disc/bluray_backend_libbluray.rs
```

behind the existing `BlurayBackend` trait.

Budget a separate wrapper phase for:

* `BLURAY*` lifetime management
* title-info allocation/freeing
* chapter/title/playlist info translation
* title selection
* read/seek state
* error conversion
* runtime version reporting
* Nix package wiring
* optional libaacs/libbdplus linking
* unsafe API audit
* tests that compare oxideav and libbluray results for the same disc

When using `libbluray-sys`, disable default features initially unless Phase 6 specifically requires them:

```toml
libbluray-sys = { version = "=1.0.1", default-features = false }
```

Then enable AACS/BD+ linkage only as explicit tonepoet features.

## Main engineering risks

### Risk 1: oxideav disc compatibility

Mitigation: backend seam from Phase 0, validation corpus, and defined fallback triggers.

### Risk 2: LPCM bit depth not surfaced cleanly

Mitigation: CLPI inspection first, PES-header probe second, materialization failure before realization if still unknown.

### Risk 3: multi-clip PTS discontinuity

Mitigation: use oxideav’s title-level PTS continuity data and map PES PTS to title PTS before chapter filtering.

### Risk 4: compressed elementary streams not accepted by ffmpeg

Mitigation: write codec-specific elementary scratch files first; fallback to chapter-bounded `.m2ts` with stream mapping.

### Risk 5: AACS complexity leaks into baseline

Mitigation: `oxideav-bluray = { default-features = false }` in Phase 0–5; add AACS only under an explicit feature.

### Risk 6: sidecar identity collisions

Mitigation: sidecar presentation identity must include playlist number, PID, stream index, and angle.

## Acceptance criteria for first usable Blu-ray release

The first non-experimental Blu-ray release should support:

* unencrypted BDMV directory and ISO sources
* playlist browsing
* primary audio stream selection
* chapter-per-track extraction
* LPCM to WAV without ffmpeg
* AC-3 and DTS through ffmpeg
* at least one lossless compressed codec through ffmpeg if fixture coverage exists
* sidecar save/load
* MusicBrainz synthetic TOC lookup
* clear errors for encrypted or unsupported discs
* no regressions in SACD, DVD-Audio, or DVD-Video
