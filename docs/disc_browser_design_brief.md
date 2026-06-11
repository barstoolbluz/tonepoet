# Disc Browser & Group Selection — Design Brief

## Purpose

Design a unified disc browsing experience for tonepoet's TUI and CLI that
lets users inspect optical disc ISOs (DVD-Audio, DVD-Video, Blu-ray, SACD),
see what presentations/groups/tracks are available, and select what to
extract — before queuing for conversion.

This brief covers DVD-Audio group selection and SACD area selection
(both immediate — extraction already works for both formats) and lays
the architectural foundation for DVD-Video and Blu-ray support (future).

---

## 1. The Problem

DVD-Audio discs contain multiple presentations of the same album:

```
MGLETSGETITON (DVD-A):
  Stream 1: 8 tracks — 96kHz/24-bit 5.0 multichannel
  Stream 2: 8 tracks — 192kHz/24-bit stereo
  Stream 3: 2 tracks — 48kHz/16-bit bonus
  Stream 4: 9 tracks — 44.1kHz/16-bit excerpts

Talking Heads 77:
  Stream 1: 13 tracks — 96kHz/24-bit 5.1 multichannel
  Stream 2: 13 tracks — 96kHz/24-bit stereo
```

SACDs have the same pattern:

```
Dark Side of the Moon (SACD):
  Stream 1: DSD64 Stereo (10 tracks, 43:00)
  Stream 2: DSD64 5.0 Multichannel (10 tracks, 43:00)
```

Today tonepoet extracts group 1 / stereo area by default. DVD-Audio has
no way to choose in the TUI; SACD has `--area` on the CLI only. Users
need to:
1. See what's on the disc before converting
2. Choose which presentation to extract
3. Optionally extract multiple presentations

The same problem applies to DVD-Video (multiple audio tracks per title:
LPCM, AC3, DTS commentary) and Blu-ray (TrueHD, DTS-HD MA, multiple
language tracks). The disc browser and stream picker designed here will
extend to those formats — same UX, different backend parsers and
extraction paths.

---

## 2. Unified Disc Model

All disc formats map to the same browsing abstraction:

```
DiscSource (ISO file or directory)
  └─ DiscContents
       ├─ format: DiscFormat (DvdAudio | DvdVideo | BluRay | Sacd)
       ├─ label: Option<String>  (volume name, provider ID, etc.)
       ├─ presentations: Vec<DiscPresentation>
       └─ copy_protection: CopyProtectionSummary

DiscPresentation
  ├─ id: PresentationId (format-specific: group nr, title nr, playlist)
  ├─ label: String ("5.1 Multichannel Mix", "Stereo", "Commentary")
  ├─ codec: String ("MLP", "LPCM", "AC3", "DTS-HD MA", "TrueHD")
  ├─ sample_rate: Option<u32>
  ├─ bit_depth: Option<u32>
  ├─ channels: ChannelSummary (count + layout label)
  ├─ tracks: Vec<DiscTrack>
  ├─ total_duration: Duration
  └─ lossless: bool

DiscTrack
  ├─ number: u32
  ├─ title: Option<String>  (from sidecar/metadata, often empty for DVD-A)
  ├─ duration: Duration
  └─ format_note: Option<String>  (e.g., "48kHz downmix" for mixed-rate tracks)
```

### Format-specific mapping

**DVD-Audio** → `DvdaDisc`:
- Each `DvdaGroup` with resolved `TitleRef`s becomes a `DiscPresentation`
- Codec, rate, depth, channels from `AudioAttributes` of the referenced ATS
- Tracks from `AudioChapter` entries
- Label synthesized from codec + rate + channels ("MLP 96kHz 5.1")

**SACD** → `SacdMetadata`:
- Stereo area → one presentation ("DSD64 Stereo")
- Multichannel area → one presentation ("DSD64 5.0" or "DSD64 5.1")
- `SacdArea::Stereo` / `SacdArea::MultiChannel` → `PresentationId`
- Already has CLI UX via `--area stereo|multichannel`
- No new extraction code needed — just the model mapping
- The TUI disc browser unifies SACD area selection with DVD-Audio
  group selection under the same "Audio Streams" UX, replacing the
  separate `--area` flag with the common stream picker

**DVD-Video** (future):
- Each VTS title with audio streams → presentations
- Multiple audio tracks per title (LPCM, AC3, DTS) as separate presentations
- Label from audio stream attributes

**Blu-ray** (future):
- MPLS playlists → presentations
- Multiple audio tracks per playlist
- Label from CLPI metadata

---

## 3. TUI Integration

### Interaction model

The disc browsing UX works through two complementary surfaces: the
**info pane** (passive, always visible when an ISO is selected) and
the **audio streams overlay** (active, opened by the user to choose).

### Info pane: auto-probe on selection

When the user highlights a disc ISO in the Browse tab file browser:

1. The info pane (where pills like `Analyze` and `Edit Tags` appear
   today) auto-probes the ISO with the appropriate parser
2. Shows disc summary: format, label, track count, copy protection status
3. Adds an **Audio Streams** pill alongside the existing pills
4. Each stream/group is labeled with codec, sample rate, channels:
   - "MLP 96kHz 5.1 (13 tracks)"
   - "MLP 96kHz Stereo (13 tracks)"
   - "48kHz Stereo (1 track)"

For a simple disc (Hawks & Doves — one audio group):

```
┌─ Info ─────────────────────────────────────────────────┐
│ HAWKSANDDOVES.iso                                      │
│ DVD-Audio · 1 audio stream · 9 tracks · 30:33          │
│ Copy protection: MKB present, AOBs readable            │
│                                                        │
│ [Audio Streams]  [Analyze]                             │
│                                                        │
│ Stream 1: MLP 176.4kHz/24-bit Stereo (9 tracks)       │
└────────────────────────────────────────────────────────┘
```

For a multi-stream disc (MGLETSGETITON):

```
┌─ Info ─────────────────────────────────────────────────┐
│ MGLETSGETITON.iso                                      │
│ DVD-Audio · 4 audio streams · 29 tracks · 70:29        │
│ Copy protection: MKB present, AOBs readable            │
│                                                        │
│ [Audio Streams]  [Analyze]                             │
│                                                        │
│ Stream 1: MLP 96kHz/24-bit 5.0 (8 tracks, 31:50)      │
│ Stream 2: MLP 192kHz/24-bit Stereo (8 tracks, 31:48)  │
│ Stream 3: 48kHz/16-bit Stereo (2 tracks, 2:30)        │
│ Stream 4: 44.1kHz/16-bit Stereo (9 tracks, 4:21)      │
└────────────────────────────────────────────────────────┘
```

For an SACD with both stereo and multichannel areas:

```
┌─ Info ─────────────────────────────────────────────────┐
│ dark_side_of_the_moon.iso                              │
│ SACD · 2 audio streams · 10 tracks                     │
│                                                        │
│ [Audio Streams]  [Analyze]                             │
│                                                        │
│ Stream 1: DSD64 Stereo (10 tracks, 43:00)              │
│ Stream 2: DSD64 5.0 Multichannel (10 tracks, 43:00)    │
└────────────────────────────────────────────────────────┘
```

This replaces the current `--area stereo|multichannel` CLI-only
selection with the same visual stream picker used for DVD-Audio.

### Audio Streams pill → browser overlay

Pressing the **Audio Streams** pill opens a browser overlay (same
overlay system used for command mode, presets, etc.) showing the full
disc structure with track-level detail:

```
┌─ Audio Streams: MGLETSGETITON.iso ───────────────────────┐
│                                                          │
│ ▸ Stream 1: MLP 96kHz/24-bit 5.0 (8 tracks, 31:50)      │
│   Stream 2: MLP 192kHz/24-bit Stereo (8 tracks, 31:48)  │
│   Stream 3: 48kHz/16-bit Stereo (2 tracks, 2:30)        │
│   Stream 4: 44.1kHz/16-bit Stereo (9 tracks, 4:21)      │
│                                                          │
│ [Enter] Convert  [E] Expand  [Space] Toggle  [Esc] Close │
└──────────────────────────────────────────────────────────┘
```

Expanding a stream:

```
│ ▾ Stream 1: MLP 96kHz/24-bit 5.0 (8 tracks, 31:50)      │
│     1. Track 01                           4:52           │
│     2. Track 02                           3:30           │
│     3. Track 03                           4:01           │
│     ...                                                  │
│   Stream 2: MLP 192kHz/24-bit Stereo (8 tracks, 31:48)  │
```

### Context menu on ISO files

Right-clicking (or pressing the context menu key on) a disc ISO in the
file browser shows:

```
┌────────────────────────────┐
│ Convert (default stream)   │
│ Browse Audio Streams...    │
│ Convert Stream ▸           │
│   Stream 1: 5.0 96kHz     │
│   Stream 2: Stereo 192kHz │
│   Stream 3: Stereo 48kHz  │
│   Stream 4: Stereo 44.1kHz│
│ Analyze                    │
│ Properties                 │
└────────────────────────────┘
```

The **Convert Stream** submenu lets the user go directly from browse
to conversion with a specific stream — no intermediate screen needed.

### Flow: stream selection → conversion

**Quick path** (context menu or Audio Streams overlay):
1. User right-clicks ISO → "Convert Stream" → picks "Stream 1: 5.0 96kHz"
2. If a preset is active: item queues immediately with that stream + preset
3. If no preset: opens Convert screen with stream pre-selected

**Browse path** (info pane → overlay):
1. User selects ISO in file browser
2. Info pane shows disc summary
3. User opens Audio Streams overlay
4. Picks a stream, presses Enter
5. Opens Convert screen with that stream loaded in the source pane

**Convert screen integration**:
When a disc ISO is loaded in the Convert screen's source pane with a
selected stream:
- Source pane shows: "MGLETSGETITON.iso · Stream 1: MLP 96kHz 5.0"
- A **Stream** pill in the source pane lets the user switch streams
  without going back to Browse
- Format/rate/depth pills in the output pane work as normal
- Queue button enqueues with the selected stream

**Direct convert** (no stream selection):
- If user selects "Convert (default stream)" or drags an ISO to the
  Convert screen without choosing a stream, the materializer uses
  default behavior (group 1 / first audio stream)
- This is what happens today — backward compatible

### What the user sees for each stream

Each stream/group label is derived from the `DiscPresentation` model:

```
[codec] [sample_rate] [channel_layout] ([track_count] tracks, [duration])
```

Channel layout uses human labels derived from the DVD-Audio channel
assignment table:

| Code | Channels | Label |
|------|----------|-------|
| 0 | 1 | Mono |
| 1 | 2 | Stereo |
| 9 | 5 | 5.0 |
| 12 | 6 | 5.1 |
| 20 | 6 | 5.1 |
| etc. | | |

The codec is known from IFO audio attributes for single-format ATS,
or "Unknown" for multi-format ATS until Phase 3 demux resolves it.
Since we now confirm codec from MLP major-sync during extraction,
the info pane can show "MLP" for all our test discs.

---

## 4. CLI Surface

### Immediate (DVD-Audio)

```bash
# List what's on a disc
tonepoet dvda-info /path/to/disc.iso

# Convert default group (group 1)
tonepoet convert disc.iso --format flac --output /tmp/out

# Convert specific group
tonepoet convert disc.iso --format flac --output /tmp/out --dvda-group 2

# Convert all groups
tonepoet convert disc.iso --format flac --output /tmp/out --dvda-group all

# Prefer stereo presentation
tonepoet convert disc.iso --format flac --output /tmp/out --dvda-group stereo

# Prefer multichannel
tonepoet convert disc.iso --format flac --output /tmp/out --dvda-group multichannel

# Override CPPM detection
tonepoet convert disc.iso --format flac --output /tmp/out --dvda-assume-decrypted
```

### Future (generic disc info)

```bash
# Generic disc probe (auto-detects DVD-A/DVD-V/BD/SACD)
tonepoet disc-info /path/to/disc.iso

# DVD-Video with audio track selection
tonepoet convert dvd.iso --format flac --output /tmp/out --dvdv-title 1 --dvdv-audio lpcm

# Blu-ray with playlist selection
tonepoet convert bd.iso --format flac --output /tmp/out --bd-playlist 00001
```

---

## 5. Implementation Phases

### Phase 4a: CLI group selection (small, immediate)

Wire `--dvda-group` CLI flag through to `SourceOptions.dvda_group_selection`:
- Add clap arg to convert subcommand
- Map string values: number → `Group(n)`, "all" → `All`,
  "stereo" → `PreferStereo`, "multichannel" → `PreferMultichannel`
- Wire `--dvda-assume-decrypted` the same way
- Add `dvda-info` subcommand that probes an ISO and prints structure

This is maybe 50 lines of CLI code + the info subcommand.

### Phase 4b: Disc probe abstraction

Create the `DiscContents` / `DiscPresentation` / `DiscTrack` model:
- Define the common types
- Implement `DvdaDisc → DiscContents` mapping
- Implement `SacdMetadata → DiscContents` mapping (SACD already works)
- The `disc-info` CLI command uses this model for display

### Phase 4c: TUI disc browser

Build the disc contents view in the Browse tab:
- Detect disc ISOs during filesystem browsing
- Probe on selection
- Render presentations and tracks
- Group selection interaction
- Queue selected presentation(s) for conversion

### Phase 4d: Convert screen integration

Add disc context to the Convert screen:
- Source pane shows disc info when an ISO is loaded
- Group picker (pill selector or overlay)
- Track selection within group

### Future phases:
- DVD-Video IFO parser + VOB demuxer
- Blu-ray MPLS/CLPI parser + M2TS demuxer
- Unified disc browser supporting all formats

---

## 6. What exists today

### DVD-Audio (fully working)

- `parse_dvda_volume()` → `DvdaDisc` with groups, titles, tracks, audio attributes
- `DvdaGroupSelection` enum: Default, Group(u8), All, PreferStereo, PreferMultichannel, PreferHighestResolution
- `SourceOptions.dvda_group_selection` field (always Default today, no CLI wiring)
- `SourceOptions.dvda_assume_decrypted` field (always false, no CLI wiring)
- Full extraction pipeline: materialize → demux → MLP decode → encode
- Tested: 192kHz stereo, 176.4kHz stereo, 96kHz 5.0, 96kHz 5.1

### SACD (fully working)

- `parse_sacd_iso()` → `SacdMetadata` with stereo/multichannel areas
- `--area stereo|multichannel` CLI flag
- Already has the "pick a presentation" UX pattern

### TUI

- Browse tab exists (filesystem browser)
- Convert screen with source/metadata/format/output panes
- PillState<T> generic selector widget
- Overlay/modal system
- Vi command mode

---

## 7. Design questions for the reasoning model

1. Should the disc browser be a **mode within Browse tab** or a **separate
   overlay/screen**? The Browse tab currently browses files — inserting
   disc contents inline vs switching to a dedicated view.

2. Should group selection in the Convert screen use a **pill selector**
   (like format/rate/depth) or a **dedicated disc info pane** that replaces
   the metadata pane when the source is a disc?

3. For multi-group extraction (e.g., "extract both stereo and multichannel"):
   should this be **one queue item with multiple groups** or **multiple
   queue items** (one per group)?

4. How should **track titles** work for DVD-Audio when IFO has no text?
   The disc browser would show "Track 01", "Track 02" etc. unless a
   sidecar provides titles. Is that acceptable UX, or should we implement
   sidecar/MusicBrainz lookup first?

5. For the `disc-info` CLI subcommand: should it output **human-readable
   text** (like `tonepoet check-tools`), **JSON** (for scripting), or
   **both** (default human, `--json` flag)?

6. Should the unified `DiscPresentation` model be in the main crate or
   in a shared library crate? It needs to be usable by both the TUI and
   CLI without pulling in format-specific parser dependencies.
