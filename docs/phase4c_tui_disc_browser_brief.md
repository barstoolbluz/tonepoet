# Phase 4c: TUI Disc Browser — Implementation Brief

## Purpose

Wire the `DiscContents` unified disc model (from Phase 4b) into the TUI
so users can browse disc ISOs, see their structure, select streams, and
queue them for conversion — all without leaving the TUI.

This brief provides the reasoning model with exact integration points,
patterns to follow, and architectural constraints.

---

## 1. What exists today

### DiscContents model (`src/disc/`)

Phase 4b delivered a complete unified disc model:

```rust
DiscContents {
    format: DiscFormat,                        // DvdAudio | Sacd
    label: String,                             // disc title or file stem
    source_path: PathBuf,
    presentations: Vec<DiscPresentation>,       // curated, no placeholders
    suppressed: Vec<SuppressedPresentation>,    // filtered placeholders
    copy_protection: CopyProtectionSummary,
    diagnostics: Vec<DiscDiagnostic>,
}

DiscPresentation {
    id: PresentationId,                        // DvdAudioGroup(u8) | SacdArea(SacdAreaId)
    label: String,                             // "MLP 96kHz/24-bit 5.0"
    format: AudioPresentationFormat,           // codec, rate, depth, channels, layout
    tracks: Vec<DiscTrack>,                    // number, title, duration
    total_duration_secs: f64,
}
```

Mappers: `disc::dvda_mapper::map_dvda_disc()` and `disc::sacd_mapper::map_sacd_disc()`.

AOB probe: `disc::dvda_utils::probe_group_aob_format()` reads one sector
per group to determine MLP vs LPCM codec.

### SACD handling in the TUI (the pattern to follow)

SACD ISOs are already browsable in the TUI. The implementation spans
these exact locations:

**Detection:**
- `EntryKind::SacdIso` variant (browse.rs line 50)
- `BrowseState.sacd_classify_cache` (browse.rs line 531)
- `upgrade_iso_kinds()` (browse.rs line 911) — post-scan pass that
  reclassifies `.iso` Archive entries to `SacdIso` via `is_sacd_iso()`
- `BrowseEntry.is_sacd_iso()` and `is_probeable()` (browse.rs lines 408, 420)

**Probing:**
- `probe_audio()` (probe.rs line 228) — branches on `is_sacd_iso()` to
  call `probe_sacd()` instead of ffmpeg
- `probe_sacd()` (probe.rs) — parses ScarletBook TOC, builds `SourceInfo`
- `read_metadata_sacd()` (probe.rs) — reads album metadata from TOC + sidecar

**Info pane:**
- `entry_info_lines()` (draw_browse.rs line 1271) — `SacdIso` arm shows
  format, codec, rate, channels, duration, size, album metadata, Edit Tags pill

**Browse → Convert:**
- `SourceMode::from_single()` (app.rs line 940) — detects SACD, parses
  tracks, builds `SourceMode::MultiTrack` with `area_label`
- `MultiTrack` variant carries track list for the selected area

**Context menu:**
- `build_browse_entry_menu()` (context_menu.rs line 468) — `SacdIso` arm
  with Edit metadata, Select, Tagging submenu, Utilities submenu

### What the TUI does NOT have for disc browsing

- No `EntryKind::DvdAudioIso` variant
- No DVD-Audio ISO detection in `upgrade_iso_kinds()`
- No DVD-Audio branch in `probe_audio()` or `entry_info_lines()`
- No DVD-Audio branch in `SourceMode::from_single()`
- No Audio Streams overlay (for either SACD or DVD-Audio)
- No stream/group picker in the Convert screen
- No disc-specific context menu actions

---

## 2. What to implement

### 2a. DVD-Audio ISO detection in Browse tab

**Add `EntryKind::DvdAudioIso` variant** to `browse.rs` (line 50).

**Add a classify cache** to `BrowseState`:
```rust
pub dvda_classify_cache: HashMap<PathBuf, (SystemTime, bool)>,
```

**Extend `upgrade_iso_kinds()`** (browse.rs line 911) to check for
DVD-Audio ISOs alongside SACD. Detection order: SACD first (cheaper —
3 short reads of 8 bytes each), then DVD-Audio (needs UDF/ISO9660 filesystem probe for
`AUDIO_TS/AUDIO_TS.IFO`).

DVD-Audio directory detection: if the browse path is a directory
containing `AUDIO_TS/AUDIO_TS.IFO`, the parent directory entry should
also be recognized as browsable disc content. This is a separate
detection path from ISO files.

**Add helpers** on `BrowseEntry`:
```rust
pub fn is_dvda_iso(&self) -> bool
```
Update `is_probeable()` to include `DvdAudioIso`.

**DVD-Audio ISO detection function** — needs a lightweight check that
doesn't do a full parse. Options:
- Try `IsoUdfDvdaVolume::open()` then check for AUDIO_TS.IFO (moderate cost)
- Check the first few sectors for UDF/ISO9660 structure + DVDAUDIO-AMG magic
  (the materializer's `detect_dvda_source()` does this but is private)
- Create a new `pub fn is_dvda_iso(path: &Path) -> bool` in `src/tui/dvda/`
  or `src/disc/dvda_utils.rs` that wraps the detection

The function should be fast enough for browse-tab scanning (called once
per `.iso` file in a directory listing). `is_sacd_iso()` takes ~3 seeks;
the DVD-Audio check will be slightly more expensive (UDF parse) but
still sub-millisecond for a single file.

### 2b. Info pane for disc ISOs

**Add a `DvdAudioIso` branch** to `entry_info_lines()` in draw_browse.rs
(after the `SacdIso` branch at line 1271).

For DVD-Audio ISOs, the info pane should show:
```
DVD-Audio ISO
4 audio streams · 27 tracks
Copy protection: MKB present, AOBs readable

Stream 1: MLP 96kHz/24-bit 5.0 (8 tracks, 31:50)
Stream 2: MLP 192kHz/24-bit Stereo (8 tracks, 31:48)
Stream 3: LPCM 48kHz/16-bit Stereo (2 tracks, 2:30)
Stream 4: LPCM 44.1kHz/16-bit Stereo (9 tracks, 4:21)

[Audio Streams]  [Analyze]
```

This requires the disc to be parsed (not just detected). The info pane
rendering is synchronous, so the disc parse + AOB probe should happen
asynchronously when the cursor moves to the ISO entry, with the result
cached.

**Async disc probe pattern:**
1. On cursor move to a disc ISO, check `probe_cache` for a cached `DiscContents`
2. If not cached, spawn an async task that:
   - Opens the volume
   - Parses the disc (`parse_dvda_volume` or `parse_sacd_iso`)
   - Probes AOBs (for DVD-Audio)
   - Maps to `DiscContents` via the mapper
   - Sends result back via `AppMessage::DiscProbeComplete`
3. Cache the result in `probe_cache` (or a new `disc_probe_cache`)
4. Re-render the info pane with the cached result

**New `AppMessage` variant:**
```rust
DiscProbeComplete {
    path: PathBuf,
    result: Box<Result<DiscContents, String>>,
}
```

**SACD info pane unification:** The existing `SacdIso` info pane branch
could also be refactored to use `DiscContents` instead of raw
`SourceInfo` / `SourceMetadata`. This would unify the info pane
rendering for both disc types. However, this is optional — the existing
SACD info pane works and doesn't need to change unless the unification
simplifies the code.

**New `TuiButton` variant:**
```rust
BrowseInfoAudioStreams,
```

**New pill:** `[Audio Streams]` — opens the disc browser overlay when
clicked. Only shown for disc ISOs with 2+ presentations. For single-
presentation discs, the pill is hidden (per the design brief's
"single-stream optimization").

### 2c. Audio Streams overlay

**New `ActiveOverlay` variant:**
```rust
DiscBrowser(Box<DiscBrowserState>),
```

**`DiscBrowserState` struct:**
```rust
pub struct DiscBrowserState {
    pub contents: DiscContents,
    pub cursor: usize,                         // selected presentation index
    pub expanded: Vec<bool>,                   // per-presentation expand/collapse
    pub scroll: usize,
    pub source_path: PathBuf,                  // for passing to Convert
}
```

**Overlay rendering** (new function in draw_overlays.rs):

The overlay shows a bordered box titled "Audio Streams: filename.iso":

```
┌─ Audio Streams: MGLETSGETITON.iso ─────────────────┐
│                                                      │
│ ▸ Stream 1: MLP 96kHz/24-bit 5.0 (8 tracks, 31:50) │
│   Stream 2: MLP 192kHz/24-bit Stereo (8 tracks)     │
│   Stream 3: LPCM 48kHz/16-bit Stereo (2 tracks)     │
│   Stream 4: LPCM 44.1kHz/16-bit Stereo (9 tracks)   │
│                                                      │
│ [Enter] Convert  [E] Expand  [Space] Toggle  [Esc]  │
└──────────────────────────────────────────────────────┘
```

When expanded (E key):
```
│ ▾ Stream 1: MLP 96kHz/24-bit 5.0 (8 tracks, 31:50)  │
│     1. Track 01                           4:52        │
│     2. Track 02                           3:30        │
│     3. Track 03                           4:01        │
│     ...                                               │
│   Stream 2: MLP 192kHz/24-bit Stereo (8 tracks)      │
```

**Key bindings** (new handler in keybindings.rs):
- `Up`/`Down` or `j`/`k`: move cursor between presentations
- `Enter`: convert selected presentation (go to Convert screen with
  that stream pre-selected)
- `E` or `Right`: expand/collapse selected presentation's track list
- `Space`: toggle selection (for multi-stream extraction — future)
- `Esc` or `q`: close overlay

**Mouse support** (follows the two-pass `ButtonRenderMap` pattern):
- Each presentation row is a clickable target — click to select, double-
  click to convert (same as browse tab file entries)
- The expand/collapse arrow (▸/▾) is a clickable target
- Footer pills ([Enter] Convert, [E] Expand, [Esc] Close) are clickable
- Register all targets via `app.button_map` in the second rendering pass
- Add `TuiButton` variants: `DiscBrowserStream(usize)` for presentation
  rows, `DiscBrowserExpand(usize)` for expand arrows, `DiscBrowserConvert`,
  `DiscBrowserClose`

**Integration with Convert screen:**

When the user presses Enter on a presentation:
1. Close the overlay
2. Build a `SourceMode` for the Convert screen with the selected
   presentation. Options:
   a. Use existing `SourceMode::MultiTrack` with the presentation's
      tracks as the track list
   b. Add a new `SourceMode::DiscStream` variant that carries the
      `DiscPresentation` + `DiscContents` + selected presentation index

   Option (a) is simpler and reuses existing rendering. The `area_label`
   field becomes the presentation label. Track entries come from
   `DiscTrack`. The `PresentationId` needs to be carried somewhere so
   the conversion pipeline knows which group/area to extract.

3. Switch to Convert screen (`app.current_screen = AppScreen::Convert`)
4. The format pane pills auto-configure based on the presentation's
   audio format (sample rate, bit depth)

### 2d. Context menu for disc ISOs

**Add a new arm** to `build_browse_entry_menu()` in context_menu.rs for
`EntryKind::DvdAudioIso`.

Menu structure:
```
Convert (default stream)
Browse Audio Streams...
Convert Stream ▸
  Stream 1: MLP 96kHz/24-bit 5.0
  Stream 2: MLP 192kHz/24-bit Stereo
  Stream 3: LPCM 48kHz/16-bit Stereo
  Stream 4: LPCM 44.1kHz/16-bit Stereo
Edit metadata
Select
Tagging ▸
  [same as SacdIso]
File operations ▸
  [same as other entries]
```

**New `ContextAction` variants:**
```rust
BrowseDiscStreams,                         // opens the Audio Streams overlay
ConvertDiscStream(PresentationId),         // convert a specific stream
```

**Building the stream submenu** requires having the `DiscContents`
available at menu construction time. The disc probe cache (from 2b)
provides this. If the disc hasn't been probed yet, the "Convert Stream"
submenu is omitted and only "Browse Audio Streams..." appears (which
triggers probing + overlay).

### 2e. Convert screen stream picker (optional for Phase 4c)

This is Phase 4d per the original design brief. If included in 4c:

When a disc ISO is loaded in the Convert screen's source pane with a
selected stream, add a **Stream pill** in the source pane that lets the
user switch streams without going back to Browse:

```
┌─ Source ─────────────────────────────────────────────┐
│ MGLETSGETITON.iso                                    │
│ Stream: [◀ MLP 96kHz/24-bit 5.0 ▶]                 │
│ 8 tracks · 31:50                                     │
│   1. Track 01   4:52                                 │
│   ...                                                │
└──────────────────────────────────────────────────────┘
```

This requires storing the full `DiscContents` in the source state so
the pill can cycle between presentations.

---

## 3. Key types and locations

### Types to add

| Type | File | Purpose |
|------|------|---------|
| `EntryKind::DvdAudioIso` | browse.rs | Browse tab detection |
| `DiscBrowserState` | app.rs | Overlay state |
| `ActiveOverlay::DiscBrowser` | app.rs | Overlay variant |
| `AppMessage::DiscProbeComplete` | message.rs | Async probe result |
| `TuiButton::BrowseInfoAudioStreams` | button_map.rs | Info pane pill |
| `TuiButton::DiscBrowserStream(usize)` | button_map.rs | Overlay stream row click |
| `TuiButton::DiscBrowserExpand(usize)` | button_map.rs | Overlay expand arrow click |
| `TuiButton::DiscBrowserConvert` | button_map.rs | Overlay Convert pill |
| `TuiButton::DiscBrowserClose` | button_map.rs | Overlay Close pill |
| `ContextAction::BrowseDiscStreams` | context_menu.rs | Menu action |
| `ContextAction::ConvertDiscStream` | context_menu.rs | Menu action |

### Functions to add

| Function | File | Purpose |
|----------|------|---------|
| `is_dvda_iso(path)` | disc/dvda_utils.rs or tui/dvda/ | Lightweight detection |
| `spawn_disc_probe(path, tx)` | browse.rs | Async disc parse + map |
| `draw_disc_browser(f, state)` | draw_overlays.rs | Overlay rendering |
| `handle_disc_browser_key(key, state)` | keybindings.rs | Overlay key handler |
| `build_dvda_iso_menu(entry, disc_cache)` | context_menu.rs | Context menu builder |
| `entry_info_lines` DvdAudioIso arm | draw_browse.rs | Info pane rendering |

### Functions to modify

| Function | File | Change |
|----------|------|--------|
| `upgrade_iso_kinds()` | browse.rs | Add DVD-Audio detection |
| `probe_audio()` | probe.rs | Add DVD-Audio branch |
| `SourceMode::from_single()` | app.rs | Add DVD-Audio detection |
| `entry_info_lines()` | draw_browse.rs | Add DvdAudioIso arm |
| `build_browse_entry_menu()` | context_menu.rs | Add DvdAudioIso arm |
| `handle_overlay_key()` | keybindings.rs | Add DiscBrowser dispatch |
| `draw_overlay()` | draw_overlays.rs | Add DiscBrowser dispatch |
| `handle_message()` | event_loop.rs | Add DiscProbeComplete handler |
| `render_entry_line()` | draw_browse.rs | Add DvdAudioIso color/style (SacdIso uses PURPLE) |
| `type_label()` | browse.rs | Add DvdAudioIso label (e.g. "dvda") |
| `classify_file()` | browse.rs | ISO files initially classified as Archive; upgrade_iso_kinds reclassifies post-scan |

---

## 4. Architectural constraints

### Two-pass rendering

The TUI uses two-pass rendering: first draw (immutable state reference),
then register mouse buttons (mutable `button_map`). The disc browser
overlay must follow this pattern — draw the presentation list first,
then register clickable items.

### Async probing

Disc parsing (especially with AOB probes) can take 50-200ms. This must
happen on a `spawn_blocking` task, not on the main event loop thread.
The result arrives via `AppMessage::DiscProbeComplete` and is cached.

### State ownership

The `DiscContents` model is `Clone`, so it can be stored in:
- `BrowseState` probe cache (for info pane)
- `DiscBrowserState` (for the overlay)
- `SourceMode` (for the Convert screen)

### PresentationId bridging

When a user selects a presentation for conversion, the `PresentationId`
must be carried through to the conversion pipeline:
- `PresentationId::DvdAudioGroup(n)` maps to
  `SourceOptions.dvda_group_selection = DvdaGroupSelection::Group(n)`
- `PresentationId::SacdArea(SacdAreaId::Stereo)` maps to
  `SourceOptions.sacd_area = Some(SacdArea::Stereo)`
- `PresentationId::SacdArea(SacdAreaId::MultiChannel)` maps to
  `SourceOptions.sacd_area = Some(SacdArea::MultiChannel)`

This bridge is needed in `ConversionItem` or `PipelineRequest`
construction.

---

## 5. What the reasoning model should decide

1. **Detection function placement**: should `is_dvda_iso()` live in
   `src/disc/dvda_utils.rs`, `src/tui/dvda/`, or as a method on
   `DirectoryDvdaVolume`/`IsoUdfDvdaVolume`? It needs to be fast and
   available to both the browse tab and the probe system.

2. **SourceMode variant**: use existing `MultiTrack` for disc streams,
   or add a new `DiscStream` variant? `MultiTrack` already handles SACD
   with `area_label` and track lists, but doesn't carry a `DiscContents`
   or a `PresentationId`. Adding fields to `MultiTrack` vs a new variant.

3. **Disc probe cache**: new field on `BrowseState` (e.g.,
   `disc_cache: HashMap<PathBuf, DiscContents>`), or reuse the existing
   `probe_cache` with a wrapper enum?

4. **SACD unification**: should the existing SACD info pane / Browse
   handling be refactored to use `DiscContents`, or left as-is? Pros:
   unified rendering code. Cons: risk of breaking working SACD flow.

5. **Overlay vs inline**: should the stream list be rendered as an
   overlay (modal, centered box) or inline in the info pane (always
   visible when a disc is selected)? The design brief suggests an
   overlay opened by an "Audio Streams" pill.

6. **Phase 4c vs 4d boundary**: should the Convert screen stream picker
   (Source pane Stream pill) be included in 4c, or deferred to 4d?

7. **Directory-based DVD-Audio detection**: when browsing a directory
   that contains `AUDIO_TS/AUDIO_TS.IFO`, should the directory itself
   be recognized as a disc? Should there be a separate
   `EntryKind::DvdAudioDir` variant, or should directories with
   `AUDIO_TS/` reuse `DvdAudioIso`? How does the browse tab represent
   a directory-as-disc vs a directory-as-folder?

8. **Color assignment**: what color should `DvdAudioIso` entries use in
   the browse list? SACD uses `theme::PURPLE`. Same color (unified
   "disc" color) or a distinct color?

---

## 6. Test corpus

### DVD-Audio fixtures (IFO only, tests detection + parse, no AOB probe)

| Disc | Presentations | Suppressed |
|------|--------------|------------|
| hdad2009 | 2 | 0 |
| mgletsgetiton | 4 | 2 |
| hawks_and_doves | 1 | 1 |
| talking_heads_77 | 2 | 1 |

### DVD-Audio ISOs (full, at /mnt/scratch/dev/dawdiolab/test-isos/)

Same discs plus AP I Robot, AP Eye in the Sky, AP Friendly Card, plus
new discs confirmed working: Running on Empty, Harvest, Close to the
Edge, Fragile.

### SACD ISOs

Confirmed working: Johnny Cash — At Folsom Prison (2 areas: Stereo +
5.1 Multichannel, 19 tracks each).

---

## 7. Existing code to read

Before implementing, read these files to understand the exact patterns:

```
src/tui/app.rs            — AppState, SourceMode, ActiveOverlay, ConvertState
src/tui/browse.rs         — BrowseState, EntryKind, upgrade_iso_kinds, classify cache
src/tui/draw_browse.rs    — entry_info_lines, info pane rendering, pill registration
src/tui/context_menu.rs   — ContextAction, build_browse_entry_menu, SacdIso arm
src/tui/draw_overlays.rs  — overlay pattern: draw_overlay dispatch, per-overlay functions
src/tui/keybindings.rs    — handle_overlay_key dispatch, handle_browse_key, load_browse_selection
src/tui/event_loop.rs     — handle_message dispatch, AudioProbeComplete handler
src/tui/message.rs        — AppMessage variants
src/tui/button_map.rs     — TuiButton variants
src/tui/probe.rs          — probe_audio SACD branch, probe_sacd, read_metadata_sacd
src/tui/pill.rs           — PillState<T> for stream picker
src/disc/                 — DiscContents model, mappers, labels, dvda_utils
```
