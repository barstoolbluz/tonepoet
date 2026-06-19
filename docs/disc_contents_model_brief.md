# DiscContents Unified Disc Model — Design Brief

## Purpose

Define a unified data model (`DiscContents`) that represents the browsable
structure of any optical disc ISO — DVD-Audio, SACD, and future DVD-Video
and Blu-ray. The model sits between format-specific parsers (which produce
rich, format-native types) and consumers (TUI disc browser, CLI `disc-info`,
Convert screen stream picker) which need a common abstraction.

This brief asks the reasoning model to make architectural decisions about
model placement, filtering policy, and the boundary between the model and
its consumers.

---

## 1. What exists today

### DVD-Audio parser → `DvdaDisc`

The `dvda-demuxer` crate produces a deeply hierarchical model:

```
DvdaDisc
  ├─ amg: AmgInfo (audio_title_table with per-group track counts, durations)
  ├─ title_sets: Vec<TitleSet>
  │     ├─ audio_formats: Vec<AudioAttributes> (rate, depth, channels per format slot)
  │     ├─ titles: Vec<AudioTitle>
  │     │     └─ chapters: Vec<AudioChapter> (track_nr, len_in_pts, sector_ranges)
  │     └─ aobs: Vec<AobFileEntry>
  ├─ samg: Option<SamgInfo> (per-track format from AUDIO_PP.IFO)
  ├─ groups: Vec<DvdaGroup>
  │     ├─ group_nr, title_refs, samg_tracks
  │     └─ correlation: GroupCorrelation
  ├─ copy_protection: CopyProtectionInfo
  └─ diagnostics: Vec<DvdaDiagnostic>
```

Key facts:
- Groups are the user-visible "presentations" but include placeholders
  (1-track / 0:01 duration menu entries)
- Multi-format title sets can't be resolved from IFO alone — need AOB probe
- The AOB probe (`probe_group_aob_format()` in main.rs) reads one sector
  per group to determine codec (MLP vs LPCM) and exact format
- `AudioCoding` is a single-variant `Unknown` enum in the parser — codec
  is always determined at extraction time or via AOB probe
- `GroupCorrelation` tracks provenance: `FromAmgAott`, `FromAtsiFallback`,
  `SamgOnly`, `MixedAmgAndSamg`

### SACD parser → `SacdMetadata`

```
SacdMetadata
  ├─ master_toc: MasterToc (catalog, genres, date, hybrid flag)
  ├─ master_text: Option<SacdText> (album title/artist/publisher)
  ├─ stereo: Option<AreaInfo>
  │     ├─ header: AreaTocHeader (kind, channel_count, total_playtime, track_count)
  │     └─ tracks: Vec<TrackEntry> (start_lsn, duration, text, isrc, genre)
  ├─ multi_channel: Option<AreaInfo> (same structure)
  └─ consistency: TocConsistencyReport
```

Each `AreaInfo` also carries a `TocConsistencyReport` for per-area
diagnostic data (redundant copy validation, sector range checks).

Key facts:
- Always exactly 0-2 presentations (stereo area, multichannel area)
- No filtering needed — both areas are always real content
- Rich per-track metadata (title, performer, composer, arranger, ISRC)
- DSD-only: sample rate is always 2,822,400 Hz (DSD64)
- Sidecar XML (`SidecarMetadata`) is the primary metadata source when present
- `PlayTime` uses minutes/seconds/frames at 75fps

### Current CLI display (`dvda-info`)

The `run_dvda_info()` function in main.rs directly walks `DvdaDisc` with
no intermediate model. It uses:
- `probe_group_aob_format()` — AOB sector probe for codec/format
- `group_format_summary()` — IFO/SAMG fallback for format
- `group_track_count()` — title_refs → chapters, SAMG, AOTT fallbacks
- `group_duration_secs()` — PTS summation with SAMG fallback
- `channel_label_from_code()` — shared 0-20 code → Mono/Stereo/X.0/X.1

### Pipeline materializers

Both `DvdaAudioMaterializer` and `SacdIsoMaterializer` produce `PreparedSource`
(with `Vec<PreparedTrack>`) for the conversion pipeline. This is a
conversion-oriented model, not a browsing model — it carries sector
ranges, source refs, and metadata needed for extraction, not for display.

---

## 2. The design question

### Thin model vs curated model

**Thin model**: `DiscContents` faithfully represents everything the parser
found. For DVD-Audio, this means 6 groups for MGLETSGETITON (including
1-track/0:01 placeholders). Consumers (TUI, CLI) decide what to show.

**Curated model**: `DiscContents` filters to "meaningful audio presentations."
Placeholder groups are excluded at mapping time. Consumers get a clean list.

Arguments for thin:
- Model stays simple, no policy decisions baked in
- Future formats (DVD-Video, Blu-ray) have different filtering criteria
- Consumers can always filter further; the model can't un-filter
- Diagnostic tools (like `dvda-info`) might want to see everything

Arguments for curated:
- The TUI disc browser and CLI show the same filtered view
- Every consumer would duplicate the same filtering logic
- "Is this a real presentation?" is a property of the disc, not the viewer
- The design brief sketches 4 streams for MGLETSGETITON, not 6

### Where does filtering live?

If the model is thin, filtering must happen somewhere. Options:

1. **In each consumer** — TUI browser, CLI `disc-info`, Convert screen
   all apply the same filter independently. Duplication.

2. **As a method on `DiscContents`** — `disc.meaningful_presentations()`
   returns a filtered view while the full list is still accessible via
   `disc.presentations`. Both views available, no data lost.

3. **As a separate filtering pass** — a `fn curate(raw: DiscContents)
   -> DiscContents` that produces a second, filtered model. Clean
   separation but two models in memory.

### Placeholder detection heuristics (DVD-Audio specific)

The test corpus shows these placeholder characteristics:
- 1 track, 0:01 duration (Hawks & Doves group 2, Talking Heads group 7)
- MGLETSGETITON groups 2 and 4: 1 track, 0:01, appear to be menu/gap entries

Possible heuristic: a group is a placeholder if:
- `track_count == 1 AND duration < 5 seconds`
- OR `track_count == 0`

This would reduce MGLETSGETITON from 6 groups to 4, Hawks & Doves from 2
to 1, Talking Heads from 3 to 2. All matching the design brief's expected
output. Single-format discs (HDAD2009, AP discs) are unaffected.

But edge cases exist:
- A legitimate bonus track could be short
- Placeholder groups in MGLETSGETITON (groups 2 and 4) have
  `GroupCorrelation::MixedAmgAndSamg` — they carry real SAMG track data
  alongside their 1-track/0:01 AOTT entry. A heuristic that counts
  tracks via SAMG instead of AOTT would NOT flag them as placeholders.
  The heuristic must use the AOTT-derived track count (from title_refs →
  chapters), not the SAMG track count.
- The heuristic needs validation against a larger corpus.

---

## 3. Model placement

### Option A: In the main crate (`src/disc/`)

New module `src/disc/mod.rs` with the model types. Accessible from both
TUI and CLI code. Can import from `dvda-demuxer` and `sacd` modules.

### Option B: In a new shared crate (`crates/tonepoet-disc/`)

Standalone crate with no TUI dependencies. Mappers live alongside the
model. Clean dependency graph but adds a workspace member.

### Option C: In the pipeline module (`src/convert/pipeline/disc.rs`)

Close to the existing pipeline types. But `DiscContents` is a browsing
model, not a conversion model — this creates a semantic mismatch.

---

## 4. What the model needs to carry

### Per-disc

| Field | DVD-Audio source | SACD source | DVD-Video (future) | Blu-ray (future) |
|-------|-----------------|-------------|--------------------|--------------------|
| Format | `DiscFormat::DvdAudio` | `DiscFormat::Sacd` | `DiscFormat::DvdVideo` | `DiscFormat::BluRay` |
| Label | `amg.provider_identifier` (often empty) | `master_text.album_title` | ? | ? |
| Presentations | mapped from `groups` | mapped from areas | mapped from VTS titles | mapped from MPLS |
| Copy protection | `CopyProtectionInfo` | none (no CPPM) | CSS/region | AACS/BD+ |
| Source path | the ISO or directory | the ISO | the ISO | the ISO/directory |

### Per-presentation

| Field | DVD-Audio source | SACD source |
|-------|-----------------|-------------|
| ID | `group_nr` | `AreaKind` |
| Label | synthesized: "MLP 96kHz/24-bit 5.0" | synthesized: "DSD64 Stereo" |
| Codec | from AOB probe or "Unknown" | always "DSD" |
| Sample rate | from AudioAttributes/AOB probe | 2,822,400 Hz (DSD64) |
| Bit depth | from AudioAttributes/AOB probe | 1-bit (DSD) |
| Channels | from ChannelAssignment | from AreaTocHeader.channel_count |
| Channel layout | from assignment code 0-20 | from loudspeaker_config |
| Track count | from chapters or SAMG or AOTT | from AreaTocHeader.track_count |
| Total duration | from PTS sums | from AreaTocHeader.total_playtime |
| Lossless | true (MLP/LPCM are lossless) | true (DSD is native format) |

### Per-track

| Field | DVD-Audio source | SACD source |
|-------|-----------------|-------------|
| Number | chapter.track_nr (1-based) | 1-based index in area.tracks |
| Title | none from IFO; sidecar/MusicBrainz | TrackEntry.text.title |
| Duration | chapter.len_in_pts / 90,000 | TrackEntry.duration.total_seconds() |
| Format note | e.g. "48kHz downmix" for mixed-rate | e.g. "DST encoded" |

---

## 5. AOB probe integration

The `dvda-info` CLI already probes AOBs to determine codec and format.
The `DiscContents` mapper for DVD-Audio should do the same probe —
otherwise multi-format title sets show "Unknown" codec.

Question: should the AOB probe happen:
- **During mapping** (`DvdaDisc → DiscContents`)? This means the mapper
  needs the volume handle, making it impure (I/O during model construction).
- **Before mapping** as a separate enrichment pass? Probe results are
  attached to the `DvdaDisc` or passed alongside it to the mapper.
- **After mapping** as a `DiscContents` enrichment? The model starts with
  "Unknown" and gets refined. Two-phase construction.

The existing `dvda-info` code does the probe per-group in the display
loop. Moving it into the mapper would centralize it.

---

## 6. What the reasoning model should decide

1. **Thin vs curated**: should `DiscContents.presentations` include
   placeholder groups, or should they be filtered at mapping time?
   If thin, should there be a `meaningful_presentations()` accessor?

2. **Placeholder heuristic**: if filtering, what criteria? Duration
   threshold? Track count? Correlation type? How to validate against
   the corpus?

3. **Model placement**: main crate module, new crate, or pipeline module?

4. **AOB probe timing**: during mapping, before mapping, or after mapping?

5. **Label synthesis**: who builds the human-readable presentation label
   ("MLP 96kHz/24-bit 5.0")? The mapper, or the consumer?

6. **Crate dependencies**: should the model crate depend on `dvda-demuxer`
   and the SACD parser, or should mappers live outside the model crate?

7. **`disc-info` CLI command**: should this replace `dvda-info`, or
   coexist? `disc-info` would auto-detect format and delegate.

8. **Relationship to `PreparedSource`**: the pipeline already has
   `PreparedSource` / `PreparedTrack`. Should `DiscContents` be
   convertible to `PreparedSource`, or are they independent models
   serving different purposes (browsing vs conversion)?

9. **Diagnostics**: both `DvdaDisc` and `SacdMetadata` carry diagnostic
   data (`DvdaDiagnostic`, `TocConsistencyReport`). Should `DiscContents`
   carry diagnostics for display in the TUI or `disc-info` CLI? Or
   should diagnostics stay on the format-native types only?

---

## 7. Test corpus for validation

### DVD-Audio fixtures (IFO only, no AOBs)

| Disc | Groups | Expected presentations | Placeholders |
|------|--------|-----------------------|-------------|
| hdad2009 | 2 | 2 (both 192/24 stereo) | 0 |
| ap_i_robot | 2 | 2 (both 192/24 stereo) | 0 |
| ap_friendly_card | 2 | 2 (both 192/24 stereo) | 0 |
| ap_eye_in_the_sky | 2 | 2 (both 192/24 stereo) | 0 |
| hawks_and_doves | 2 | 1 (176.4/24 stereo) | 1 (group 2: 1 track, 0:01) |
| talking_heads_77 | 3 | 2 (96/24 5.1 + 96/24 stereo) | 1 (group 7: 1 track, 0:01) |
| mgletsgetiton | 6 | 4 (96/24 5.0, 192/24 stereo, 48/16 stereo, 44.1/16 stereo) | 2 (groups 2,4: 1 track, 0:01) |

### DVD-Audio ISOs (full, at /mnt/scratch/dev/dawdiolab/test-isos/)

Same 7 discs as above but with AOBs for real format probing.

### SACD ISOs

Available via the existing SACD test infrastructure. Each has 1-2 areas
(stereo, multichannel), no placeholders, rich per-track metadata.
