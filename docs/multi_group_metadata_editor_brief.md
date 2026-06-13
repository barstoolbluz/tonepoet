# Multi-Group Metadata Editor — Design Brief

## Purpose

Add group/presentation tabs to the metadata editor overlay so users
can tag multiple presentations of the same disc in one session. When
populating from MusicBrainz, the user can apply tags to one, some,
or all presentations. Each presentation's tags are independent but
can be bulk-populated from the same lookup.

This replaces the current behavior where MusicBrainz tags land on
only one group's tracks, leaving other groups untagged.

---

## 1. The problem

### DVD-Audio: Brothers in Arms

The disc has two presentations:
- Group 1 (ATS 1): 5.1 multichannel, 9 tracks
- Group 3 (ATS 2): stereo, 9 tracks (same album)

MusicBrainz tagging opens the editor for one group (group 3). Tags
are written to metabase track IDs `2.1.*`. The user then converts
group 1 — track IDs `1.1.*` — which have no tags. Result: output
files are named "Track 01", "Track 02", etc.

### SACD: similar pattern

SACDs have stereo and multichannel areas. The existing mirror system
(`save_sacd_sidecar` with `mirror_sibling_area`) copies tags across
areas, but this is automatic and invisible — the user can't review
or customize per-area tags.

### The fix

The metadata editor should show tabs for each presentation, letting
the user:
1. See which presentations exist
2. Switch between them to view/edit per-group tags
3. Populate one or all from MusicBrainz in one action
4. Save — writes to the format-native sidecar for each group

---

## 2. Existing precedent: AccurateRip disc tabs

The AccurateRip overlay already uses tabs for multi-disc batch runs.
The pattern:
- Tab bar at the top of the overlay: `[Disc 1] [Disc 2] [Disc 3]`
- Clicking/keying a tab switches the content below
- Each tab has independent state

The metadata editor tabs would follow the same visual pattern but
represent presentations within a single disc, not separate discs.

---

## 3. Design

### Tab source

Tabs come from `DiscContents.presentations`. Each tab shows:
- DVD-Audio: "Group 1: MLP 96kHz/24-bit 5.1"
- SACD: "Stereo" / "Multichannel"
- Future DVD-Video: "LPCM Stereo" / "AC3 5.1"

For single-presentation discs, no tab bar is shown (same as today).

### Editor state

The `MetadataEditorState` carries per-presentation tag data:

```
MetadataEditorState {
    // Existing fields for the active presentation...
    entries: Vec<TagEntry>,
    paths: Vec<PathBuf>,
    ...

    // New: multi-group support
    presentation_tabs: Vec<PresentationTab>,
    active_tab: usize,
}

PresentationTab {
    id: PresentationId,
    label: String,           // "Group 1: MLP 96kHz/24-bit 5.1"
    entries: Vec<TagEntry>,  // per-track tag data for this group
    dirty: bool,
}
```

Switching tabs swaps the active `entries` into the tab state and
loads the new tab's entries.

### MusicBrainz population

When the user runs `:tags-mb` from within the multi-group editor:

1. MB lookup uses the active tab's track count/durations for TOC
2. Results populate the active tab's entries
3. A prompt asks: "Apply to all groups?" (Y/N)
   - Yes: copy the same tags to all other tabs with matching track count
   - No: only the active tab gets the tags

### Save behavior

On save, write the format-native sidecar:

**DVD-Audio**: Write all tabs' tags to the metabase XML. Each tab's
entries map to track IDs `{titleset}.{title}.{track}` for that
presentation's group.

**SACD**: Write all tabs' tags to the SACD sidecar XML. Each tab's
entries map to area-indexed tracks (area 1 = stereo, area 2 =
multichannel). This replaces the existing automatic mirror system
with explicit per-area control.

The sidecar format does NOT change — the editor just writes tags
for multiple groups/areas into the same sidecar file.

Both formats already support multi-group data in a single file:
- DVD-Audio metabase: tracks from different groups coexist keyed by
  `{titleset}.{title}.{track}` (e.g., Morph the Cat has `1.1.*`
  multichannel and `1.2.*` stereo, each with distinct ALBUM values)
- SACD sidecar: tracks are sequentially numbered with stereo first
  (1..N), multichannel after (N+1..2N), split by TOTALTRACKS boundary

### Input bindings

Keyboard:
- `Tab` / `Shift+Tab`: cycle between presentation tabs
- `1`-`9`: jump to tab by number (if not in edit mode)
- All existing editor keys work within the active tab

Mouse:
- Each tab in the tab bar is a clickable target (register via
  `ButtonRenderMap` in the two-pass rendering pattern)
- Click a tab to switch to it
- Add `TuiButton::MetadataEditorTab(usize)` variant for tab clicks

### Compatibility constraints

- **SACD sidecar XML**: must remain parseable by foobar2000's
  Super Audio CD Decoder plugin. The format is unchanged — both
  areas' tracks already coexist in the same XML, keyed by track ID.
- **DVD-Audio metabase XML**: must remain parseable by foobar2000's
  foo_input_dvda plugin. The format is unchanged — tracks from
  multiple groups already coexist in the same XML.
- No new sidecar formats. No format merging.

---

## 4. Implementation scope

### What to modify

| File | Change |
|------|--------|
| `src/tui/app.rs` | Add `PresentationTab` struct, extend `MetadataEditorState` |
| `src/tui/draw_overlays.rs` | Render tab bar above editor content |
| `src/tui/keybindings.rs` | Tab switching, "apply to all" prompt |
| `src/tui/command.rs` | Build multi-tab editor for DVD-Audio and SACD |
| `src/tui/dvda_metabase.rs` | Write multi-group tags on save |
| `src/tui/sacd_sidecar.rs` | Write multi-area tags on save (replace mirror) |

### What NOT to change

- Sidecar file formats (SACD XML, DVD-Audio metabase XML)
- `DiscContents` model
- Single-file metadata editor (non-disc sources)
- CLI `tags-mb` command

---

## 5. Code to read

```
Existing metadata editor:
  src/tui/app.rs              — MetadataEditorState, TagEntry
  src/tui/draw_overlays.rs    — draw_metadata_editor
  src/tui/keybindings.rs      — handle_metadata_editor_key, save logic

AccurateRip tabs (precedent):
  src/tui/draw_overlays.rs    — AccurateRip multi-disc tab rendering
  src/tui/keybindings.rs      — tab switching in AR overlay

Disc tagging:
  src/tui/command.rs          — TagsFromMb DVD-Audio and SACD handlers
  src/tui/keybindings.rs      — open_metadata_editor_for_sacd,
                                 open_metadata_editor_for_dvda_group
  src/tui/dvda_metabase.rs    — write_metabase, group_track_addrs
  src/tui/sacd_sidecar.rs     — save_sacd_sidecar, mirror_sibling_area

Unified disc model:
  src/disc/model.rs           — DiscContents, DiscPresentation, PresentationId
```

---

## 6. What the reasoning model should produce

1. Extended `MetadataEditorState` with presentation tab support
2. Tab bar rendering in the metadata editor overlay
3. Tab switching key handlers
4. "Apply to all groups" prompt after MusicBrainz population
5. Multi-group save for both DVD-Audio metabase and SACD sidecar
6. Modified `TagsFromMb` handlers to build multi-tab editors
7. Tests for tab switching and multi-group save round-trip
